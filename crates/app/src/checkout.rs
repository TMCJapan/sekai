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
use std::sync::Arc;
use std::sync::mpsc::{self, TrySendError};
use std::time::{Duration, Instant};

use sekai_core::{ChunkCoord, ChunkHash, CommitId, RegionKey, Scope};
use sekai_storage::{MetaStore, SekaiStore, StorageError};
use sekai_util::{JobSet as _, ThreadRunner, WorkerPanic};
use sekai_world::{Region, RegionFile, WorldLock, WorldLockMode, WorldTree};

use crate::error::AppError;
use crate::instance::{WorldGuard, WorldHandleMut};
use crate::progress::{BuildProgress, Progress};
use crate::regions::discover_regions;
use crate::workers::worker_error;

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

/// Checkout restore policy.
///
/// `Default` is strict: commit-unknown files are deleted, tombstoned
/// chunks vanish, missing blobs abort, and missing files resolve
/// sibling-first. The rebuild fan-out follows the instance's runner.
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

impl<W: WorldTree, S: SekaiStore, T: ThreadRunner> WorldHandleMut<'_, W, S, T>
where
    AppError: From<W::Error>,
{
    /// Rebuild the bound world from `commit`, returning execution report
    /// and per-phase timings.
    ///
    /// Only `scope` is rebuilt: region files outside the scope are never
    /// written, deleted, or otherwise touched. `progress_builder` counts the
    /// files to write or delete, stepping on the async side as each one
    /// finishes, never on the runner's threads.
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
        let runner = &self.instance.runner;
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

        // Discovery walks the filesystem, so it runs on the runner with
        // its own share of the world lock, like every file write.
        let discover_started = Instant::now();
        let world_handle = world.clone();
        let discover_guard = Arc::clone(&guard);
        let mut discovered = runner
            .call(move || {
                let _guard = discover_guard;
                discover_regions(&world_handle)
            })
            .await
            .map_err(worker_error("checkout worker"))??;
        let discover_dt = discover_started.elapsed();
        discovered.retain(|key, _| scope.matches_region(*key));

        let files_started = Instant::now();
        let jobs = plan_files(groups, &discovered, world, options)?;
        let report = run_files(
            jobs,
            store.blob(),
            runner,
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

/// Run every job at the runner's parallelism: the caller fetches each
/// file's blobs and hands the prepared write to the pool through a bounded
/// channel, so at most one prepared image per slot is in flight and a
/// finished file frees its slot for the next.
///
/// Write outcomes are small, so they drain as they land - during the feed
/// and after it - keeping `progress` and failure surfacing live while the
/// fetches continue. A failed fetch or write stops the feed; the writes
/// already running keep their share of the world lock until they end, and
/// the run waits for them before returning.
async fn run_files<R, L, C>(
    jobs: Vec<FileJob<R>>,
    cas: &C,
    runner: &impl ThreadRunner,
    options: CheckoutOptions,
    timestamp: u32,
    guard: &Arc<WorldGuard<L>>,
    progress_builder: &impl BuildProgress,
) -> Result<CheckoutReport, AppError>
where
    R: RegionFile,
    L: WorldLock,
    C: sekai_storage::BlobStore<Error = StorageError>,
    AppError: From<R::Error>,
{
    let mut progress = progress_builder.build_progress(jobs.len());
    let mut report = CheckoutReport {
        files_written: 0,
        files_deleted: 0,
        chunks_restored: 0,
    };
    let (tx, rx) = mpsc::sync_channel::<WriteJob<R>>(runner.parallelism());
    let write_guard = Arc::clone(guard);
    let mut writes = runner.run(rx, move |job| {
        let _guard = Arc::clone(&write_guard);
        write_file(job, options, timestamp)
    });
    let fed = feed_files(
        jobs,
        cas,
        options,
        &tx,
        &mut writes,
        &mut report,
        &mut progress,
    )
    .await;
    // Cut the channel off before joining or abandoning the pool: a worker
    // blocked on the receiver only retires once the sender is gone.
    drop(tx);
    let mut outcome = fed;
    if outcome.is_err() {
        writes.cancel();
    }
    // Drain to retirement even after a failure: the workers hold shares of
    // the world lock, and the run must not return (letting the caller retry
    // or release the world) until every one of them has ended.
    while let Some(joined) = writes.join_next().await {
        if outcome.is_ok() {
            outcome = joined
                .map_err(worker_error("checkout worker"))
                .and_then(|result| result)
                .map(|done| {
                    count_outcome(done, &mut report);
                    progress.step();
                });
            if outcome.is_err() {
                writes.cancel();
            }
        }
    }
    outcome?;
    Ok(report)
}

/// Fetch each file's blobs on the caller's executor and feed the prepared
/// writes to the pool, joining completions whenever every slot is busy.
#[allow(clippy::too_many_arguments)]
async fn feed_files<R, C>(
    jobs: Vec<FileJob<R>>,
    cas: &C,
    options: CheckoutOptions,
    tx: &mpsc::SyncSender<WriteJob<R>>,
    writes: &mut impl sekai_util::JobSet<Result<Outcome, AppError>>,
    report: &mut CheckoutReport,
    progress: &mut impl Progress,
) -> Result<(), AppError>
where
    R: RegionFile,
    C: sekai_storage::BlobStore<Error = StorageError>,
    AppError: From<R::Error>,
{
    for job in jobs {
        // Fetch pass: the blobs reach the caller's executor here; the
        // rebuild itself (builder, live merge, write) runs on the pool.
        let prepared = match job {
            FileJob::Delete(region) => WriteJob::Delete(region),
            FileJob::Rebuild { key, region, rows } => {
                let mut chunks = Vec::with_capacity(rows.len());
                let mut blob_buf = Vec::new();
                for (coord, hash) in &rows {
                    if fetch_or_skip(cas, hash, options.on_missing_blob, &mut blob_buf).await? {
                        chunks.push((coord.x, coord.z, blob_buf.clone()));
                    }
                }
                WriteJob::Rebuild {
                    key,
                    region,
                    chunks,
                }
            }
        };
        // Feed with backpressure: when every slot is busy, join one write
        // before handing over the next image.
        let mut prepared = Some(prepared);
        while let Some(job) = prepared.take() {
            while let Some(joined) = writes.try_join_next() {
                count_outcome(joined.map_err(worker_error("checkout worker"))??, report);
                progress.step();
            }
            match tx.try_send(job) {
                Ok(()) => {}
                Err(TrySendError::Full(job)) => {
                    prepared = Some(job);
                    let joined = writes.join_next().await.ok_or_else(pool_stopped)?;
                    count_outcome(joined.map_err(worker_error("checkout worker"))??, report);
                    progress.step();
                }
                Err(TrySendError::Disconnected(_)) => return Err(pool_stopped()),
            }
        }
    }
    Ok(())
}

/// The write pool retired while the feed still had work for it.
fn pool_stopped() -> AppError {
    AppError::Storage(StorageError::backend(
        "checkout worker",
        WorkerPanic::new("write pool stopped early"),
    ))
}

/// Fold one finished write into the report.
const fn count_outcome(outcome: Outcome, report: &mut CheckoutReport) {
    match outcome {
        Outcome::Written { chunks } => {
            report.files_written += 1;
            report.chunks_restored += chunks;
        }
        Outcome::Deleted => report.files_deleted += 1,
        Outcome::AlreadyAbsent => {}
    }
}

/// One file's write, prepared by the fetch pass: the blobs are already in
/// hand, so the whole job is synchronous.
enum WriteJob<R: RegionFile> {
    Delete(R),
    Rebuild {
        key: RegionKey,
        region: R,
        chunks: Vec<(i32, i32, Vec<u8>)>,
    },
}

/// Carry out one file's write on a runner thread holding a share of the
/// world lock until it ends (ADR-0022): an abandoned checkout cannot free
/// the world while a file is still being written.
fn write_file<R: RegionFile>(
    job: WriteJob<R>,
    options: CheckoutOptions,
    timestamp: u32,
) -> Result<Outcome, AppError>
where
    AppError: From<R::Error>,
{
    match job {
        WriteJob::Delete(region) => {
            if !region.exists() {
                return Ok(Outcome::AlreadyAbsent);
            }
            region.remove()?;
            Ok(Outcome::Deleted)
        }
        WriteJob::Rebuild {
            key,
            region,
            chunks,
        } => {
            let mut builder = sekai_anvil::RegionBuilder::new(key.rx, key.rz, timestamp)?;
            let mut restored: BTreeSet<(i32, i32)> = BTreeSet::new();
            for (x, z, payload) in &chunks {
                builder.stage_chunk(*x, *z, payload)?;
                restored.insert((*x, *z));
            }
            if options.keep_post_commit_chunks || options.keep_tombstoned_chunks {
                merge_live_chunks(&region, key, &restored, options, &mut builder)?;
            }
            region.write(&builder.image()?)?;
            Ok(Outcome::Written {
                chunks: chunks.len(),
            })
        }
    }
}

/// What one file job did to its region file.
#[derive(Clone, Copy)]
enum Outcome {
    /// Rebuilt from `chunks` restored chunks.
    Written { chunks: usize },
    /// Removed.
    Deleted,
    /// Due for deletion, but gone by the time its job ran.
    AlreadyAbsent,
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
