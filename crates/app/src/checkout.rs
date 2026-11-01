//! Checkout execution: rebuild world files from a resolved commit plan.
//!
//! Rationale: the restore set (which blobs each region file needs) comes
//! from the [`core`](sekai_core::usecase::checkout) plan; what stays here
//! is mechanics `core` cannot own: world-layout discovery, path
//! derivation, and the atomic MCA rewrite itself. Region files are rebuilt
//! wholesale from CAS blobs (never patched). Under the default
//! [`CheckoutOptions`] on-disk chunks unknown to the commit (created
//! later) vanish along with post-commit regions, all-tombstone regions
//! delete their file instead of leaving a header-only shell, and a blob
//! missing from CAS aborts loudly: that is corruption, and writing a
//! partial world would be worse than writing none. Keep-policies in
//! [`CheckoutOptions`] relax each of those strict behaviors explicitly.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use sekai_core::{BlobHash, CheckoutReport, ChunkCoord, CommitId, RegionKey, Scope};
use sekai_storage::{SekaiStore, StorageError};
use sekai_world::{Region, RegionFile, WorldLock, WorldLockMode, WorldTree};

use crate::error::AppError;
use crate::instance::{ExclusiveWorldGuard, WorldGuard, WorldHandleMut};
use crate::regions::discover_regions;

/// Per-phase timings for checkout execution.
#[derive(Debug, Clone, Default)]
pub struct CheckoutTimings {
    /// Wall-clock total.
    pub total: Duration,
    /// Checkout plan resolution from metadata.
    pub plan: Duration,
    /// Region discovery.
    pub discover: Duration,
    /// Rebuilding MCA files, restoring chunks from CAS, and removing post-commit files.
    pub checkout_files: Duration,
}

/// Per-file progress report for the `progress` callback.
#[derive(Debug, Clone, Copy)]
pub struct CheckoutProgress {
    /// Files fully rebuilt so far.
    pub files_done: usize,
    /// Files to rebuild in total.
    pub files_total: usize,
    /// Chunks restored so far.
    pub chunks_done: usize,
}

/// What to do when a commit blob is absent from CAS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MissingBlobPolicy {
    /// Abort loudly: a missing blob is corruption, and writing a partial
    /// world would be worse than writing none.
    #[default]
    Abort,
    /// Skip the chunk, leaving it out of the rebuilt file.
    SkipChunk,
}

/// Where to rebuild a commit-known region whose file is missing on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MissingFilePolicy {
    /// Prefer a same-dimension sibling's directory, falling back to the
    /// layout-derived location.
    #[default]
    SiblingFirst,
    /// Always use the layout-derived location, ignoring siblings.
    DerivedOnly,
    /// Fail loudly instead of guessing a location.
    Error,
}

/// Checkout restore policy. `Default` is strict: commit-unknown files
/// are deleted, tombstoned chunks vanish, missing blobs abort, and
/// missing files resolve sibling-first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CheckoutOptions {
    /// Keep region files unknown to the commit instead of deleting them.
    pub keep_post_commit_files: bool,
    /// Keep live bytes for chunks unknown to the commit (created
    /// afterwards inside a commit-known region) instead of dropping them.
    pub keep_post_commit_chunks: bool,
    /// Keep live bytes for commit-tombstoned chunks instead of removing
    /// them. Fully tombstoned region files are left alone rather than
    /// deleted.
    pub keep_tombstoned_chunks: bool,
    /// How to handle a commit blob missing from CAS.
    pub on_missing_blob: MissingBlobPolicy,
    /// Where to rebuild a commit-known region whose file is missing.
    pub on_missing_file: MissingFilePolicy,
}

impl<W: WorldTree, S: SekaiStore> WorldHandleMut<'_, W, S>
where
    AppError: From<W::Error>,
{
    /// Rebuild the bound world from `commit`, returning execution report
    /// and per-phase timings.
    ///
    /// Only `scope` is rebuilt: region files outside the scope are never
    /// written, deleted, or otherwise touched. `progress` fires as files
    /// complete, always on the async side: a callback that works straight
    /// on the future's own state must be safe to run there.
    ///
    /// Rebuilding writes (and may delete) region files, so it goes through
    /// `&mut self`: only one write session per world can exist at a time.
    pub async fn checkout(
        &mut self,
        commit: CommitId,
        options: CheckoutOptions,
        scope: Scope,
        progress: impl FnMut(CheckoutProgress),
    ) -> Result<(CheckoutReport, CheckoutTimings), AppError> {
        let guard =
            ExclusiveWorldGuard::acquire(self.world, WorldLockMode::Exclusive)?.into_shared();
        let store = &self.instance.store;
        let world = &*self.world;
        let total_started = Instant::now();

        let plan_started = Instant::now();
        // The commit's creation time stamps every rebuilt file.
        let plan = sekai_core::usecase::checkout::plan_checkout(store.meta(), commit)
            .await
            .map_err(AppError::Checkout)?;
        let plan_dt = plan_started.elapsed();

        let timestamp = u32::try_from(plan.created_at_ms / 1000).unwrap_or(u32::MAX);

        // Scope before the file passes: out-of-scope regions are dropped
        // from both sides, so the passes below cannot tell they exist.
        let mut groups = plan.groups;
        groups.retain(|key, _| scope.matches_region(*key));
        let mut tombstones = plan.tombstones;
        tombstones.retain(|key, _| scope.matches_region(*key));

        // Discovery walks the filesystem: clone the handle and run it on
        // the blocking pool, owning a share of the world lock like every
        // file pass.
        let discover_started = Instant::now();
        let world_handle = world.clone();
        let discovery_guard = Arc::clone(&guard);
        let mut discovered = tokio::task::spawn_blocking(move || {
            let _guard = discovery_guard;
            discover_regions(&world_handle)
        })
        .await??;
        let discover_dt = discover_started.elapsed();
        discovered.retain(|key, _| scope.matches_region(*key));

        let job: CheckoutJob<S, W> = CheckoutJob {
            groups,
            tombstones,
            options,
            cas: store.cas().clone(),
            world: world.clone(),
            discovered,
            guard,
        };

        let files_started = Instant::now();
        let report = checkout_file_pass(job, timestamp, progress).await?;
        let files_dt = files_started.elapsed();

        let timings = CheckoutTimings {
            total: total_started.elapsed(),
            plan: plan_dt,
            discover: discover_dt,
            checkout_files: files_dt,
        };

        Ok((report, timings))
    }
}

/// Everything one checkout file pass needs.
struct CheckoutJob<S: SekaiStore, T: WorldTree> {
    groups: BTreeMap<RegionKey, Vec<(ChunkCoord, BlobHash)>>,
    tombstones: BTreeMap<RegionKey, Vec<ChunkCoord>>,
    options: CheckoutOptions,
    cas: S::Blobs,
    world: T,
    discovered: BTreeMap<RegionKey, T::Region>,
    /// One share per blocking step: the world stays locked for as long as
    /// *any* of them still runs, even after the future is cancelled.
    guard: Arc<WorldGuard<T::Lock>>,
}

/// Rebuild every region file: present rows from CAS blobs, tombstones and
/// post-commit files deleted unless kept by [`CheckoutOptions`].
///
/// Blob fetches are async; `progress` fires between files on the async side.
/// Each file's build and write runs on the blocking pool, never on an async
/// worker, and reports completion through [`Completion`] so a parked
/// `progress` callback does not also hold a blocking thread.
async fn checkout_file_pass<S: SekaiStore, T: WorldTree>(
    job: CheckoutJob<S, T>,
    timestamp: u32,
    mut progress: impl FnMut(CheckoutProgress),
) -> Result<CheckoutReport, AppError>
where
    AppError: From<T::Error>,
{
    let CheckoutJob {
        groups,
        tombstones,
        options,
        cas,
        world,
        discovered,
        guard,
    } = job;
    let mut key_set: BTreeSet<RegionKey> = discovered.keys().copied().collect();
    key_set.extend(groups.keys().copied());
    key_set.extend(tombstones.keys().copied());
    let keys: Vec<RegionKey> = key_set.into_iter().collect();

    let mut report = CheckoutReport {
        files_written: 0,
        files_deleted: 0,
        chunks_restored: 0,
    };
    // Tracks the steps still queued: the last one out releases the world.
    let pending = Arc::new(AtomicUsize::new(0));
    let files_total = keys.len();
    // Counts files as they finish: `done` reads it, so each report carries
    // the file it concludes rather than a running total.
    let mut files_done = 0usize;
    let mut blob_buf = Vec::new();
    #[allow(clippy::explicit_counter_loop)]
    for key in keys {
        files_done += 1;
        let done = |chunks_done| CheckoutProgress {
            files_done,
            files_total,
            chunks_done,
        };
        let rows = groups.get(&key);
        let tombs = tombstones.get(&key);
        let region = match discovered.get(&key) {
            Some(region) => region.clone(),
            None => match rows {
                // No rows and no file: nothing to do.
                None => {
                    progress(done(report.chunks_restored));
                    continue;
                }
                Some(_) => resolve_target(&discovered, &world, &key, options.on_missing_file)?,
            },
        };
        let Some(rows) = rows else {
            // No present rows: either fully tombstoned at the commit or
            // created afterwards. Keep-policies leave the live file alone;
            // strict checkout deletes it.
            let keep = if tombs.is_some() {
                options.keep_tombstoned_chunks
            } else {
                options.keep_post_commit_files
            };
            let step = Step::start(
                Arc::clone(&guard),
                queued(
                    Arc::clone(&pending),
                    move |reported| -> Result<(), AppError> {
                        let deleted = !keep && region.exists();
                        if deleted {
                            region.remove()?;
                        }
                        let _ = reported.send(deleted);
                        Ok(())
                    },
                ),
            );
            report.files_deleted += usize::from(step.outcome().await?);
            progress(done(report.chunks_restored));
            continue;
        };
        let mut writer = sekai_anvil::RegionBuilder::new(key.rx, key.rz, timestamp)?;
        let mut restored: BTreeSet<(i32, i32)> = BTreeSet::new();
        for (coord, hash) in rows {
            if !fetch_or_skip(&cas, hash, options.on_missing_blob, &mut blob_buf).await? {
                continue;
            }
            writer.stage_chunk(coord.x, coord.z, &blob_buf)?;
            restored.insert((coord.x, coord.z));
            report.chunks_restored += 1;
        }
        let keep_live = options.keep_post_commit_chunks || options.keep_tombstoned_chunks;
        let to_merge = tombs.cloned();
        let step = Step::start(
            Arc::clone(&guard),
            queued(
                Arc::clone(&pending),
                move |reported| -> Result<(), AppError> {
                    if keep_live {
                        merge_live_chunks(
                            &region,
                            key,
                            &restored,
                            to_merge.as_deref(),
                            options,
                            &mut writer,
                        )?;
                    }
                    region.write(&writer.image()?)?;
                    let _ = reported.send(());
                    Ok(())
                },
            ),
        );
        step.outcome().await?;
        report.files_written += 1;
        progress(done(report.chunks_restored));
    }
    // Every step has been awaited, so nothing can start after this: release
    // the world rather than waiting for the guard to drop with the future.
    release_if_idle(&guard, &pending);
    Ok(report)
}

/// Counts a step in as queued and closes over the exit that counts it out.
///
/// The counter lets a cancelled future - which never reaches its own
/// release - still free the world once the last step is done. Counting
/// inside the closure means a step torn down without running cannot leave
/// the counter stuck.
fn queued<R, F>(
    pending: Arc<AtomicUsize>,
    work: F,
) -> impl FnOnce(tokio::sync::oneshot::Sender<R>) -> Result<(), AppError> + Send + 'static
where
    R: Send + 'static,
    F: FnOnce(tokio::sync::oneshot::Sender<R>) -> Result<(), AppError> + Send + 'static,
{
    pending.fetch_add(1, Ordering::SeqCst);
    move |reported| {
        let out = work(reported);
        let _ = pending.fetch_sub(1, Ordering::SeqCst);
        out
    }
}

/// Release the world when no step is queued any more.
fn release_if_idle<L>(guard: &Arc<WorldGuard<L>>, pending: &AtomicUsize) {
    if pending.load(Ordering::SeqCst) == 0 {
        guard.release();
    }
}

/// One blocking file step and the result it reports.
///
/// The step owns a share of the world lock, so the lock outlives the future
/// that started it. Completion travels over a oneshot instead of the join
/// handle: a `progress` callback parked on the async side would otherwise
/// keep a blocking thread busy while the step is already done, and a
/// cancelled future must still see the lock released.
struct Step<R, T> {
    finished: tokio::task::JoinHandle<Result<T, AppError>>,
    reported: tokio::sync::oneshot::Receiver<R>,
}

impl<R: Send + 'static, T: Send + 'static> Step<R, T> {
    /// Run `work` on the blocking pool, holding a share of the world for its
    /// duration.
    ///
    /// `work` sends its result through the oneshot, then returns. The share
    /// keeps the lock alive if the future is dropped before this step ends.
    fn start<L>(
        guard: Arc<WorldGuard<L>>,
        work: impl FnOnce(tokio::sync::oneshot::Sender<R>) -> Result<T, AppError> + Send + 'static,
    ) -> Self
    where
        L: WorldLock + Send + 'static,
    {
        let (sender, reported) = tokio::sync::oneshot::channel();
        let finished = tokio::task::spawn_blocking(move || {
            let _guard = guard;
            work(sender)
        });
        Self { finished, reported }
    }

    /// Wait for the step to end, then take its result.
    async fn outcome(self) -> Result<R, AppError> {
        self.finished.await??;
        self.reported.await.map_err(|_| {
            AppError::Storage(StorageError::backend(
                "checkout worker",
                std::io::Error::other("the file step stopped before reporting"),
            ))
        })
    }
}

/// Fetch one blob into `out`, honoring `policy` when CAS has no such blob.
///
/// `false` means the chunk must be skipped; under
/// [`MissingBlobPolicy::Abort`] a missing blob is corruption and propagates.
pub(super) async fn fetch_or_skip<C: sekai_core::BlobStore<Error = StorageError>>(
    cas: &C,
    hash: &BlobHash,
    policy: MissingBlobPolicy,
    out: &mut Vec<u8>,
) -> Result<bool, AppError> {
    match sekai_core::BlobStore::fetch_into(cas, hash, out).await {
        Ok(()) => Ok(true),
        Err(StorageError::BlobMissing { .. }) if policy == MissingBlobPolicy::SkipChunk => {
            Ok(false)
        }
        Err(source) => Err(source.into()),
    }
}

/// Merge live on-disk chunks that the commit does not restore into
/// `writer`: tombstoned coordinates under `keep_tombstoned_chunks`,
/// commit-unknown ones under `keep_post_commit_chunks`. Missing live
/// files contribute nothing; corrupt ones fail loudly.
fn merge_live_chunks<R: RegionFile>(
    region: &R,
    key: RegionKey,
    restored: &BTreeSet<(i32, i32)>,
    tombs: Option<&[ChunkCoord]>,
    options: CheckoutOptions,
    writer: &mut sekai_anvil::RegionBuilder,
) -> Result<(), AppError>
where
    AppError: From<R::Error>,
{
    if !region.exists() {
        return Ok(());
    }
    let image = region.read(key)?;
    let tombed: BTreeSet<(i32, i32)> = tombs
        .map(|coords| coords.iter().map(|c| (c.x, c.z)).collect())
        .unwrap_or_default();
    let mut failed = None;
    image.visit_chunks(|chunk| {
        if restored.contains(&(chunk.x, chunk.z)) {
            return true;
        }
        let keep = if tombed.contains(&(chunk.x, chunk.z)) {
            options.keep_tombstoned_chunks
        } else {
            options.keep_post_commit_chunks
        };
        if keep && let Err(source) = writer.stage_chunk(chunk.x, chunk.z, chunk.payload) {
            failed = Some(source);
            return false;
        }
        true
    })?;
    if let Some(source) = failed {
        return Err(AppError::Anvil(source));
    }
    Ok(())
}

/// Target region file for a commit-known region whose file is missing on
/// disk, per [`MissingFilePolicy`].
fn resolve_target<T: WorldTree>(
    discovered: &BTreeMap<RegionKey, T::Region>,
    world: &T,
    key: &RegionKey,
    policy: MissingFilePolicy,
) -> Result<T::Region, AppError>
where
    AppError: From<T::Error>,
{
    match policy {
        MissingFilePolicy::Error => Err(sekai_world::WorldError::UnknownRegionPath {
            dim: key.dim,
            kind: key.kind,
            region_x: key.rx,
            region_z: key.rz,
        }
        .into()),
        MissingFilePolicy::DerivedOnly => Ok(world.derive_region(*key)?),
        MissingFilePolicy::SiblingFirst => sibling_or_derived(discovered, world, key),
    }
}

/// Target region file for a commit-known region whose file is missing on
/// disk.
///
/// Prefers a same-dimension sibling's directory (folders may have moved
/// since the commit, e.g. across a 26.1 migration); only derives from the
/// resolved layout when no sibling exists. Any sibling of that dimension
/// and family serves - they share the directory - and the first one the
/// map reports is taken. Non-derivable namespaces fail loudly instead of
/// writing somewhere wrong.
fn sibling_or_derived<T: WorldTree>(
    discovered: &BTreeMap<RegionKey, T::Region>,
    world: &T,
    key: &RegionKey,
) -> Result<T::Region, AppError>
where
    AppError: From<T::Error>,
{
    if let Some(sibling) = discovered
        .iter()
        .find(|(found, _)| found.dim == key.dim && found.kind == key.kind)
        .map(|(_, region)| region)
    {
        // A tree that cannot name the sibling fails loudly: falling back
        // to the derived handle would write the region somewhere else.
        return Ok(world.sibling_region(sibling, *key)?);
    }
    Ok(world.derive_region(*key)?)
}
