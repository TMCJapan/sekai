//! Checkout planning and execution: resolve a commit's restore set, then
//! rebuild world files from it.
//!
//! Rationale: [`plan_checkout`] resolves the restore set (which blobs each
//! region file needs) straight from the store traits; what only this layer
//! can own is the mechanics beyond them: world-layout discovery, path
//! derivation, and the atomic MCA rewrite itself. Region files are rebuilt
//! wholesale from CAS blobs (never patched). Under the default
//! [`CheckoutOptions`] on-disk chunks unknown to the commit (created
//! later) vanish along with post-commit regions, regions absent from the
//! snapshot delete their file instead of leaving a header-only shell, and a
//! blob missing from CAS aborts loudly: that is corruption, and writing a
//! partial world would be worse than writing none. Keep-policies in
//! [`CheckoutOptions`] relax each of those strict behaviors explicitly.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use sekai_core::{ChunkCoord, ChunkHash, CommitId, RegionKey, Scope};
use sekai_storage::{MetaStore, SekaiStore, StorageError};
use sekai_world::{Region, RegionFile, WorldLock, WorldLockMode, WorldTree};
use tokio::task::JoinSet;

use crate::error::AppError;
use crate::instance::{WorldGuard, WorldHandleMut};
use crate::progress::{BuildProgress, Progress};
use crate::regions::discover_regions;
use crate::workers::worker_slots;

/// Outcome of one checkout run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckoutReport {
    /// Region files rewritten.
    pub files_written: usize,
    /// Region files deleted (absent from the commit's snapshot).
    pub files_deleted: usize,
    /// Chunks restored from CAS blobs.
    pub chunks_restored: usize,
}

/// Resolved restore set for one commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutPlan {
    /// Commit creation time (unix millis, informational only).
    pub created_at_ms: u64,
    /// Present rows grouped by region. A region absent here has no chunks at
    /// the commit: checkout deletes its file unless a keep-policy applies.
    pub groups: BTreeMap<RegionKey, Vec<(ChunkCoord, ChunkHash)>>,
}

/// Failures while resolving a checkout plan.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CheckoutError<M> {
    /// Metadata backend failure.
    #[error("commit metadata failed")]
    Meta(M),
    /// No commit with this ID exists.
    #[error("unknown commit: {id}")]
    UnknownCommit {
        /// Requested commit ID.
        id: CommitId,
    },
}

/// Resolve present commit rows grouped by region.
pub(crate) async fn plan_checkout<M: MetaStore>(
    meta: &M,
    commit: CommitId,
) -> Result<CheckoutPlan, CheckoutError<M::Error>> {
    let created_at_ms = meta
        .lookup_commit(commit)
        .await
        .map_err(CheckoutError::Meta)?
        .map(|c| c.created_at_ms)
        .ok_or(CheckoutError::UnknownCommit { id: commit })?;
    let mut groups: BTreeMap<RegionKey, Vec<(ChunkCoord, ChunkHash)>> = BTreeMap::new();
    meta.visit_commit_chunks(commit, |entry| {
        if let Some(blob) = entry.blob {
            groups
                .entry(RegionKey::of(entry.coord))
                .or_default()
                .push((entry.coord, blob));
        }
        true
    })
    .await
    .map_err(CheckoutError::Meta)?;
    Ok(CheckoutPlan {
        created_at_ms,
        groups,
    })
}

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

/// Checkout restore policy and worker count.
///
/// `Default` is strict: commit-unknown files are deleted, tombstoned
/// chunks vanish, missing blobs abort, and missing files resolve
/// sibling-first. It rebuilds up to one file per CPU at once.
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
    /// How many files rebuild at once. `0` means one per CPU, `1` stays
    /// sequential.
    pub concurrency: usize,
}

impl<W: WorldTree, S: SekaiStore> WorldHandleMut<'_, W, S>
where
    AppError: From<W::Error>,
{
    /// Rebuild the bound world from `commit`, returning execution report
    /// and per-phase timings.
    ///
    /// Only `scope` is rebuilt: region files outside the scope are never
    /// written, deleted, or otherwise touched. `progress_builder` counts the
    /// files to write or delete, stepping on the async side as each one
    /// finishes, never on the blocking pool.
    ///
    /// What each file needs is decided before any is touched, so refusing a
    /// missing file ([`MissingFilePolicy::Error`]) leaves the world as it
    /// was. A later failure or a cancelled checkout leaves it partly
    /// rebuilt, with every file either rebuilt or untouched, never partial;
    /// the world stays locked until every write already running has ended.
    ///
    /// Rebuilding writes (and may delete) region files, so it goes through
    /// `&mut self`: only one write session per world can exist at a time.
    pub async fn checkout(
        &mut self,
        commit: CommitId,
        options: CheckoutOptions,
        scope: Scope,
        progress_builder: &impl BuildProgress,
    ) -> Result<(CheckoutReport, CheckoutTimings), AppError> {
        let guard = WorldGuard::acquire(self.world, WorldLockMode::Exclusive)?;
        let store = &self.instance.store;
        let world = &*self.world;
        let total_started = Instant::now();

        let plan_started = Instant::now();
        // The commit's creation time stamps every rebuilt file.
        let plan = plan_checkout(store.meta(), commit)
            .await
            .map_err(AppError::Checkout)?;
        let plan_dt = plan_started.elapsed();

        let timestamp = u32::try_from(plan.created_at_ms / 1000).unwrap_or(u32::MAX);

        // Scope before the file passes: out-of-scope regions are dropped
        // from both sides, so the passes below cannot tell they exist.
        let mut groups = plan.groups;
        groups.retain(|key, _| scope.matches_region(*key));

        // Discovery walks the filesystem, so it runs on the blocking pool
        // with its own share of the world lock, like every file write.
        let discover_started = Instant::now();
        let world_handle = world.clone();
        let mut discovered =
            on_blocking_pool(Arc::clone(&guard), move || discover_regions(&world_handle)).await?;
        let discover_dt = discover_started.elapsed();
        discovered.retain(|key, _| scope.matches_region(*key));

        let files_started = Instant::now();
        let jobs = plan_files(groups, &discovered, world, options)?;
        let report = run_files(
            jobs,
            store.blob(),
            options,
            timestamp,
            &guard,
            progress_builder,
        )
        .await?;
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

/// What one region file needs.
enum FileJob<R: RegionFile> {
    Delete(R),
    Rebuild {
        key: RegionKey,
        region: R,
        rows: Vec<(ChunkCoord, ChunkHash)>,
    },
}

/// Decide what every region file needs, without touching the disk: present
/// rows are rebuilt from CAS blobs, absent and post-commit files are
/// deleted unless kept by [`CheckoutOptions`]. Files left alone get no job.
fn plan_files<T: WorldTree>(
    mut groups: BTreeMap<RegionKey, Vec<(ChunkCoord, ChunkHash)>>,
    discovered: &BTreeMap<RegionKey, T::Region>,
    world: &T,
    options: CheckoutOptions,
) -> Result<Vec<FileJob<T::Region>>, AppError>
where
    AppError: From<T::Error>,
{
    let mut keys: BTreeSet<RegionKey> = discovered.keys().copied().collect();
    keys.extend(groups.keys().copied());
    keys.into_iter()
        .filter_map(|key| {
            let rows = groups.remove(&key);
            plan_file(key, rows, discovered, world, options).transpose()
        })
        .collect()
}

/// Decide what `key`'s file needs, or `None` when it is left alone.
fn plan_file<T: WorldTree>(
    key: RegionKey,
    rows: Option<Vec<(ChunkCoord, ChunkHash)>>,
    discovered: &BTreeMap<RegionKey, T::Region>,
    world: &T,
    options: CheckoutOptions,
) -> Result<Option<FileJob<T::Region>>, AppError>
where
    AppError: From<T::Error>,
{
    if let Some(rows) = rows {
        let region = if let Some(region) = discovered.get(&key) {
            region.clone()
        } else {
            resolve_target(discovered, world, &key, options.on_missing_file)?
        };
        Ok(Some(FileJob::Rebuild { key, region, rows }))
    } else {
        // No present rows: the region is absent from the snapshot, whether
        // fully tombstoned at the commit or created afterwards. Absence is
        // the tombstone, so the two cannot be distinguished; either
        // keep-policy preserves the live file, strict checkout deletes it.
        let keep = options.keep_tombstoned_chunks || options.keep_post_commit_files;
        if keep {
            Ok(None)
        } else {
            Ok(discovered.get(&key).cloned().map(FileJob::Delete))
        }
    }
}

/// Run every job as its own task, at most `options.concurrency` at once
/// (one per CPU by default) since each holds a whole region image. A
/// finished file frees its slot for the next, and `progress` steps as each
/// one is joined.
///
/// Tasks hold the world lock only weakly while they fetch: a share is taken
/// just as a file's write or delete goes to the blocking pool. So when the
/// run fails or is cancelled, the tasks still fetching are aborted without
/// extending the lock, and only the writes already on the blocking pool
/// keep the world locked until they end.
async fn run_files<R, L, C>(
    jobs: Vec<FileJob<R>>,
    cas: &C,
    options: CheckoutOptions,
    timestamp: u32,
    guard: &Arc<WorldGuard<L>>,
    progress_builder: &impl BuildProgress,
) -> Result<CheckoutReport, AppError>
where
    R: RegionFile,
    L: WorldLock,
    C: sekai_storage::BlobStore<Error = StorageError> + Clone + Send + Sync + 'static,
    AppError: From<R::Error>,
{
    let mut progress = progress_builder.build_progress(jobs.len());
    let mut report = CheckoutReport {
        files_written: 0,
        files_deleted: 0,
        chunks_restored: 0,
    };
    let slots = worker_slots(options.concurrency);
    let spawn = |set: &mut JoinSet<Result<Outcome, AppError>>, job: FileJob<R>| {
        set.spawn(run_file(
            job,
            cas.clone(),
            options,
            timestamp,
            Arc::downgrade(guard),
        ));
    };
    let mut queued = jobs.into_iter();
    let mut set = JoinSet::new();
    for job in queued.by_ref().take(slots) {
        spawn(&mut set, job);
    }
    while let Some(joined) = set.join_next().await {
        match joined?? {
            Outcome::Written { chunks } => {
                report.files_written += 1;
                report.chunks_restored += chunks;
            }
            Outcome::Deleted => report.files_deleted += 1,
            // `guard` outlives this loop, so no joined task can find the run
            // gone; only one left running after a return can.
            Outcome::AlreadyAbsent | Outcome::Abandoned => {}
        }
        progress.step();
        if let Some(job) = queued.next() {
            spawn(&mut set, job);
        }
    }
    Ok(report)
}

/// Carry out one file's job: fetch its blobs here, then write (or delete)
/// it on the blocking pool.
async fn run_file<R, L, C>(
    job: FileJob<R>,
    cas: C,
    options: CheckoutOptions,
    timestamp: u32,
    lock: Weak<WorldGuard<L>>,
) -> Result<Outcome, AppError>
where
    R: RegionFile,
    L: WorldLock,
    C: sekai_storage::BlobStore<Error = StorageError>,
    AppError: From<R::Error>,
{
    match job {
        FileJob::Delete(region) => {
            write_while_running(&lock, move || {
                if !region.exists() {
                    return Ok(Outcome::AlreadyAbsent);
                }
                region.remove()?;
                Ok(Outcome::Deleted)
            })
            .await
        }
        FileJob::Rebuild { key, region, rows } => {
            let mut builder = sekai_anvil::RegionBuilder::new(key.rx, key.rz, timestamp)?;
            let mut restored: BTreeSet<(i32, i32)> = BTreeSet::new();
            let mut blob_buf = Vec::new();
            for (coord, hash) in &rows {
                if fetch_or_skip(&cas, hash, options.on_missing_blob, &mut blob_buf).await? {
                    builder.stage_chunk(coord.x, coord.z, &blob_buf)?;
                    restored.insert((coord.x, coord.z));
                }
            }
            let keep_live = options.keep_post_commit_chunks || options.keep_tombstoned_chunks;
            write_while_running(&lock, move || {
                if keep_live {
                    merge_live_chunks(&region, key, &restored, options, &mut builder)?;
                }
                region.write(&builder.image()?)?;
                Ok(Outcome::Written {
                    chunks: restored.len(),
                })
            })
            .await
        }
    }
}

/// Hand `work` to the blocking pool with a fresh share of the world lock,
/// unless the run that spawned this task is already gone.
async fn write_while_running<L: WorldLock>(
    lock: &Weak<WorldGuard<L>>,
    work: impl FnOnce() -> Result<Outcome, AppError> + Send + 'static,
) -> Result<Outcome, AppError> {
    match lock.upgrade() {
        Some(guard) => on_blocking_pool(guard, work).await,
        None => Ok(Outcome::Abandoned),
    }
}

/// Run `work` on the blocking pool with its own share of the world lock:
/// a cancelled future cannot stop it once started, so the world stays
/// locked until it ends.
async fn on_blocking_pool<L: WorldLock, T: Send + 'static>(
    guard: Arc<WorldGuard<L>>,
    work: impl FnOnce() -> Result<T, AppError> + Send + 'static,
) -> Result<T, AppError> {
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        work()
    })
    .await?
}

/// What one file job did to its region file.
enum Outcome {
    /// Rebuilt from `chunks` restored chunks.
    Written { chunks: usize },
    /// Removed.
    Deleted,
    /// Due for deletion, but gone by the time its job ran.
    AlreadyAbsent,
    /// Not started: the run had already failed or been cancelled.
    Abandoned,
}

/// Fetch one blob into `out`, honoring `policy` when CAS has no such blob.
///
/// `false` means the chunk must be skipped; under
/// [`MissingBlobPolicy::Abort`] a missing blob is corruption and propagates.
async fn fetch_or_skip<C: sekai_storage::BlobStore<Error = StorageError>>(
    cas: &C,
    hash: &ChunkHash,
    policy: MissingBlobPolicy,
    out: &mut Vec<u8>,
) -> Result<bool, AppError> {
    match sekai_storage::BlobStore::fetch_into(cas, hash, out).await {
        Ok(()) => Ok(true),
        Err(StorageError::BlobMissing { .. }) if policy == MissingBlobPolicy::SkipChunk => {
            Ok(false)
        }
        Err(source) => Err(source.into()),
    }
}

/// Merge live on-disk chunks that the commit does not restore into
/// `builder`: a chunk absent from a commit-known region is absent from the
/// snapshot, and absence cannot distinguish a tombstone from a post-commit
/// chunk, so either keep-policy preserves it. Missing live files contribute
/// nothing; corrupt ones fail loudly.
fn merge_live_chunks<R: RegionFile>(
    region: &R,
    key: RegionKey,
    restored: &BTreeSet<(i32, i32)>,
    options: CheckoutOptions,
    builder: &mut sekai_anvil::RegionBuilder,
) -> Result<(), AppError>
where
    AppError: From<R::Error>,
{
    if !region.exists() {
        return Ok(());
    }
    let image = region.read(key)?;
    let mut failed = None;
    image.visit_chunks(|chunk| {
        if restored.contains(&(chunk.x, chunk.z)) {
            return true;
        }
        let keep = options.keep_tombstoned_chunks || options.keep_post_commit_chunks;
        if keep && let Err(source) = builder.stage_chunk(chunk.x, chunk.z, chunk.payload) {
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
        MissingFilePolicy::Error => Err(AppError::MissingRegionFile { key: *key }),
        MissingFilePolicy::DerivedOnly => Ok(world.derive_region(*key)?),
        // Folders may have moved since the commit (e.g. across a 26.1
        // migration), so a same-dimension sibling's directory wins over
        // the layout-derived one. Any sibling of that dimension and family
        // serves - they share the directory. A tree that cannot name the
        // sibling fails loudly: falling back to the derived handle would
        // write the region somewhere else. Without a sibling, a namespace
        // the layout cannot derive fails loudly as well.
        MissingFilePolicy::SiblingFirst => match discovered
            .iter()
            .find(|(found, _)| found.dim == key.dim && found.kind == key.kind)
        {
            Some((_, sibling)) => Ok(world.sibling_region(sibling, *key)?),
            None => Ok(world.derive_region(*key)?),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sekai_core::{CommitEntry, Dimension, RefName, RegionKind};
    use sekai_storage::testing::{MemMeta, block_on};

    const OVER: Dimension = Dimension::OVERWORLD;
    const REGION: RegionKind = RegionKind::Region;

    #[test]
    fn resolves_present_rows_grouped_by_region() {
        let mut meta = MemMeta::default();
        meta.with_active_ref(&RefName::parse("main").unwrap());
        let a = ChunkCoord::new(OVER, REGION, 0, 0);
        let b = ChunkCoord::new(OVER, REGION, 1, 0);
        let gone = ChunkCoord::new(OVER, REGION, 2, 0);
        let id = block_on(meta.apply_commit_incremental(
            1_000,
            &[],
            &[
                CommitEntry::new(a, Some(ChunkHash::new([1; 32]))),
                CommitEntry::new(b, Some(ChunkHash::new([2; 32]))),
                CommitEntry::new(gone, None),
            ],
        ))
        .unwrap()
        .id;

        let plan = block_on(plan_checkout(&meta, id)).unwrap();
        assert_eq!(plan.created_at_ms, 1_000);
        assert_eq!(plan.groups.len(), 1);
        let rows = &plan.groups[&RegionKey::new(OVER, REGION, 0, 0)];
        assert_eq!(rows.len(), 2);
        assert!(rows.contains(&(a, ChunkHash::new([1; 32]))));
        assert!(rows.contains(&(b, ChunkHash::new([2; 32]))));
        assert!(
            block_on(meta.lookup_chunk(id, &gone)).unwrap().is_none(),
            "absence is the tombstone"
        );
    }

    #[test]
    fn unknown_commit_is_an_error() {
        let meta = MemMeta::default();
        assert_eq!(
            block_on(plan_checkout(&meta, CommitId([7; 32]))),
            Err(CheckoutError::UnknownCommit {
                id: CommitId([7; 32])
            })
        );
    }
}
