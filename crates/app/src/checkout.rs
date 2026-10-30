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
use std::time::{Duration, Instant};

use sekai_core::{BlobHash, CheckoutReport, ChunkCoord, CommitId, RegionKey, Scope};
use sekai_storage::{BlockingBlobStore, SekaiStore, StoreLockMode};
use sekai_world::{Region, RegionFile, WorldLockMode, WorldTree};

use crate::error::AppError;
use crate::instance::{OperationLocks, WorldHandleMut};
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
    /// complete; it must be `'static` because the file pass runs on a
    /// blocking pool (pass a `move` closure owning its state).
    ///
    /// Rebuilding writes (and may delete) region files, so it goes through
    /// `&mut self`: only one write session per world can exist at a time.
    pub async fn checkout(
        &mut self,
        commit: CommitId,
        options: CheckoutOptions,
        scope: Scope,
        progress: impl Fn(CheckoutProgress) + Send + 'static,
    ) -> Result<(CheckoutReport, CheckoutTimings), AppError> {
        let locks = OperationLocks::acquire(
            self.instance,
            StoreLockMode::Shared,
            self.world,
            WorldLockMode::Exclusive,
        )?;
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

        // Scope before the blocking pass: out-of-scope regions are dropped
        // from both sides, so the file pass below cannot tell they exist.
        let mut groups = plan.groups;
        groups.retain(|key, _| scope.matches_region(*key));
        let mut tombstones = plan.tombstones;
        tombstones.retain(|key, _| scope.matches_region(*key));

        // Discovery walks the filesystem: clone the handle and run it on
        // the blocking pool, owning a share of the locks like every
        // blocking pass.
        let discover_started = Instant::now();
        let world_handle = world.clone();
        let discovery_locks = Arc::clone(&locks);
        let mut discovered = tokio::task::spawn_blocking(move || {
            let _locks = discovery_locks;
            discover_regions(&world_handle)
        })
        .await??;
        let discover_dt = discover_started.elapsed();
        discovered.retain(|key, _| scope.matches_region(*key));

        let job = CheckoutJob {
            groups,
            tombstones,
            options,
            cas: store.blob().clone(),
            world: world.clone(),
            discovered,
            _locks: locks,
        };

        let files_started = Instant::now();
        let report =
            tokio::task::spawn_blocking(move || checkout_files(job, timestamp, progress)).await??;
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

/// Everything one checkout file pass needs, owned for the blocking task.
struct CheckoutJob<S: SekaiStore, T: WorldTree> {
    groups: BTreeMap<RegionKey, Vec<(ChunkCoord, BlobHash)>>,
    tombstones: BTreeMap<RegionKey, Vec<ChunkCoord>>,
    options: CheckoutOptions,
    cas: S::Blobs,
    world: T,
    discovered: BTreeMap<RegionKey, T::Region>,
    /// Owned by the file pass: it keeps writing after a cancelled future
    /// is dropped, and must keep the world locked until it stops.
    _locks: Arc<OperationLocks<S, T::Lock>>,
}

/// Rebuild every region file: present rows from CAS blobs, tombstones and
/// post-commit files deleted unless kept by [`CheckoutOptions`].
/// Blocking: file reads, writes, and swaps belong on a blocking pool,
/// never on an async worker.
fn checkout_files<S: SekaiStore, T: WorldTree>(
    job: CheckoutJob<S, T>,
    timestamp: u32,
    progress: impl Fn(CheckoutProgress),
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
        _locks,
    } = job;
    let mut keys: BTreeSet<RegionKey> = discovered.keys().copied().collect();
    keys.extend(groups.keys().copied());
    keys.extend(tombstones.keys().copied());

    let mut report = CheckoutReport {
        files_written: 0,
        files_deleted: 0,
        chunks_restored: 0,
    };
    let files_total = keys.len();
    let mut files_done = 0usize;
    let mut progressed = |report: &CheckoutReport| {
        files_done += 1;
        progress(CheckoutProgress {
            files_done,
            files_total,
            chunks_done: report.chunks_restored,
        });
    };
    let mut blob_buf = Vec::new();
    for key in keys {
        let rows = groups.get(&key);
        let tombs = tombstones.get(&key);
        let region = match discovered.get(&key) {
            Some(region) => region.clone(),
            None => match rows {
                None => {
                    // No rows and no file: nothing to do.
                    progressed(&report);
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
            if !keep && region.exists() {
                region.remove()?;
                report.files_deleted += 1;
            }
            progressed(&report);
            continue;
        };
        let mut writer = sekai_anvil::RegionBuilder::new(key.rx, key.rz, timestamp)?;
        let mut restored: BTreeSet<(i32, i32)> = BTreeSet::new();
        for (coord, hash) in rows {
            if !fetch_or_skip(&cas, hash, options.on_missing_blob, &mut blob_buf)? {
                continue;
            }
            writer.stage_chunk(coord.x, coord.z, &blob_buf)?;
            restored.insert((coord.x, coord.z));
            report.chunks_restored += 1;
        }
        if options.keep_post_commit_chunks || options.keep_tombstoned_chunks {
            merge_live_chunks(
                &region,
                key,
                &restored,
                tombs.map(Vec::as_slice),
                options,
                &mut writer,
            )?;
        }
        region.write(&writer.image()?)?;
        report.files_written += 1;
        progressed(&report);
    }
    Ok(report)
}

/// Fetch one blob into `out`, honoring `policy` when CAS has no such blob.
///
/// `false` means the chunk must be skipped; under
/// [`MissingBlobPolicy::Abort`] a missing blob is corruption and propagates.
pub(super) fn fetch_or_skip<C: BlockingBlobStore>(
    cas: &C,
    hash: &BlobHash,
    policy: MissingBlobPolicy,
    out: &mut Vec<u8>,
) -> Result<bool, AppError> {
    match cas.fetch_blob(hash, out) {
        Ok(()) => Ok(true),
        Err(sekai_storage::StorageError::BlobMissing { .. })
            if policy == MissingBlobPolicy::SkipChunk =>
        {
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
