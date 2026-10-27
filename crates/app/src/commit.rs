//! Recording a world state as a commit: observe files, ingest, filter the
//! delta.
//!
//! Rationale: this is the orchestration half of commit. All policy lives in
//! [`core`](sekai_core::usecase::commit): the previous-state plan, per-chunk
//! staging, delta filtering, tombstone assembly, and the atomic commit.
//! What stays here is mechanics `core` cannot own without breaking its
//! `no_std` boundary: world discovery and fingerprint observation
//! (filesystem), worker ingest on a blocking pool (threads + fsync), the
//! wall clock, and timing collection for `--timing` output.
//!
//! Crash order mirrors the core contract: every blob is flushed to CAS
//! *before* the single metadata batch commits, so a torn commit leaves at
//! most orphan blobs (reclaimed by future GC), never dangling references.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sekai_core::usecase::commit::Ingested;
use sekai_core::{
    Assembled, ChunkCoord, CommitEntry, CommitId, CommitReport, Observation, Previous,
    RegionFingerprint, RegionKey, Scope,
};
use sekai_storage::{BlockingBlobStore, SekaiStore, StoreLockMode};
use sekai_world::{RegionFile, WorldLock, WorldLockMode, WorldTree};

use crate::error::AppError;
use crate::instance::{OperationLocks, WorldHandle, WorldHandleMut};
use crate::regions::discover_regions;
use crate::time::now_ms;

/// Commit behavior knobs.
#[derive(Debug, Clone, Default)]
pub struct CommitOptions {
    /// Ingest worker count. `0` means one per CPU, `1` stays sequential.
    pub concurrency: usize,
    /// Derive volatile diff views alongside blobs. Decode failures degrade
    /// to "not computed" (`None`): the blob itself is intact, and the diff
    /// column is only a cache.
    pub with_diff: bool,
    /// Tag names ignored by diff views. `None` selects the default set.
    pub ignore_tags: Option<Vec<String>>,
}

/// Preview behavior knobs.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatusOptions {
    /// Ingest worker count. `0` means one per CPU, `1` stays sequential.
    pub concurrency: usize,
}

/// What a commit would record, without recording anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusReport {
    /// Nothing in scope differs from the latest commit.
    pub clean: bool,
    /// The active ref's target the preview is measured against, if any.
    ///
    /// `None` when that ref is unborn, even if the store already holds
    /// commits from an earlier history.
    pub latest: Option<CommitId>,
    /// In-scope regions with fresh rows (changed chunks or tombstones).
    pub changed_regions: usize,
    /// Changed regions with no stored state.
    pub new_files: usize,
    /// Stored regions gone from disk.
    pub deleted_files: usize,
    /// Chunks that would be newly recorded.
    pub new_chunks: usize,
    /// Tombstones that would be recorded for vanished chunks.
    pub tombstones: usize,
    /// Blobs absent from CAS (the rest would deduplicate).
    pub new_blobs: usize,
}

/// Per-phase timings. Informational only; never changes preview semantics.
#[derive(Debug, Clone, Default)]
pub struct StatusTimings {
    /// Wall-clock total.
    pub total: Duration,
    /// World discovery.
    pub discover: Duration,
    /// Fingerprint observation.
    pub fingerprint: Duration,
    /// Previous-state load plus ingest planning.
    pub universe_load: Duration,
    /// Opening changed region files.
    pub region_open: Duration,
    /// Visiting and hashing changed chunks (never stored).
    pub ingest: Duration,
    /// Subset of `ingest` spent hashing.
    pub hash: Duration,
}

/// Per-file progress report for the `progress` callback.
#[derive(Debug, Clone, Copy)]
pub struct CommitProgress {
    /// Files fully ingested so far.
    pub files_done: usize,
    /// Files to ingest in total.
    pub files_total: usize,
    /// Chunks ingested so far.
    pub chunks_done: usize,
}

/// Per-phase timings. Informational only; never changes commit semantics.
#[derive(Debug, Clone, Default)]
pub struct CommitTimings {
    /// Wall-clock total.
    pub total: Duration,
    /// World discovery.
    pub discover: Duration,
    /// Fingerprint observation.
    pub fingerprint: Duration,
    /// Previous-state load plus ingest planning.
    pub universe_load: Duration,
    /// Opening changed region files.
    pub region_open: Duration,
    /// Visiting, hashing, and storing changed chunks.
    pub ingest: Duration,
    /// Subset of `ingest` spent hashing.
    pub hash: Duration,
    /// Subset of `ingest` spent in `CAS put`, plus the durability barrier.
    pub cas_put: Duration,
    /// Metadata commit.
    pub db_apply: Duration,
    /// Region files skipped by file metadata. Always zero: region skipping
    /// was removed with the derived-state table.
    pub skipped_regions: usize,
    /// History rows carried over. Always zero.
    pub carried_chunks: usize,
    /// Per-region details for ingested files, ordered by region identity:
    /// dimension (by its code), then family, then coordinates - the order
    /// the world walk uses.
    pub regions: Vec<RegionTiming>,
}

/// Per-region timing slice.
#[derive(Debug, Clone)]
pub struct RegionTiming {
    /// Where the region file lives.
    pub location: String,
    /// File size in bytes.
    pub bytes: u64,
    /// Chunks ingested from the file.
    pub chunks: usize,
    /// File open time.
    pub open: Duration,
    /// Visit/hash/store time.
    pub ingest: Duration,
    /// Subset of `ingest` spent hashing.
    pub hash: Duration,
    /// Subset of `ingest` spent in `CAS put`.
    pub cas: Duration,
}

/// Changed file handle plus its fresh fingerprint, awaiting ingest.
type Changed<R> = (R, RegionFingerprint);

/// One ingested file's staged rows, ready to merge into the walk.
struct FileOutcome {
    /// Fresh fingerprint for the derived-state upsert.
    fingerprint: RegionFingerprint,
    /// History rows for the file's chunks.
    entries: Vec<CommitEntry>,
    /// Coordinates present in the file.
    present: HashSet<ChunkCoord>,
    /// Blobs newly written to CAS.
    new_blobs: usize,
    /// Per-region timing slice.
    timing: RegionTiming,
}

impl<W: WorldTree, S: SekaiStore> WorldHandleMut<'_, W, S>
where
    AppError: From<W::Error>,
{
    /// Scan the bound world and record it as a new commit in the store,
    /// additionally returning per-phase timings.
    ///
    /// The recorded timestamp is read after ingestion, when the metadata
    /// write begins.
    ///
    /// Only `scope` is ingested: out-of-scope coordinates record no rows and
    /// no tombstones, resolving through fallback from earlier commits.
    /// `progress` fires as changed files complete. The report is identical
    /// with or without timings.
    pub async fn commit(
        &mut self,
        options: CommitOptions,
        scope: Scope,
        progress: impl Fn(CommitProgress) + Send,
    ) -> Result<(CommitReport, CommitTimings), AppError> {
        let total = Instant::now();
        // Held for the whole run: blobs reach the CAS before the rows that
        // reference them, so `gc` must not scan in that window.
        let locks = OperationLocks::acquire(
            self.instance,
            StoreLockMode::Writer,
            self.world,
            WorldLockMode::Shared,
        )?;
        let store = &mut self.instance.store;

        let Prepared {
            previous,
            staged,
            walk,
            discover,
            fingerprint,
            universe_load,
        } = prepare(store, self.world, &options, &scope, progress, false, &locks).await?;

        let created_at_ms = now_ms()?;
        // Persist batched shard-directory renames before the metadata commit:
        // file data is already synced per blob, and this single barrier (at
        // most one fsync per touched shard) is what makes every referenced blob
        // crash-durable. Timed under `cas`, the durability bucket. Explicit
        // here even though workers already synced their own handles: the
        // barrier belongs at the call site, never hidden inside adapters.
        let sync_started = Instant::now();
        store.cas_mut().sync_dirs()?;
        let dir_sync = sync_started.elapsed();
        let db_started = Instant::now();
        let report = sekai_core::usecase::commit::record_commit(
            store.meta_mut(),
            &previous,
            &staged,
            created_at_ms,
        )
        .await?;
        let db_apply = db_started.elapsed();

        let timings = CommitTimings {
            total: total.elapsed(),
            discover,
            fingerprint,
            universe_load,
            region_open: walk.region_open,
            ingest: walk.ingest,
            hash: walk.hash,
            cas_put: walk.cas + dir_sync,
            db_apply,
            skipped_regions: report.skipped_regions,
            carried_chunks: report.carried_chunks,
            regions: walk.regions,
        };
        Ok((report, timings))
    }
}

/// Everything a commit records and a status preview counts, before the
/// commit decision that separates them.
struct Prepared {
    previous: Previous,
    staged: Assembled,
    walk: IngestTimings,
    discover: Duration,
    fingerprint: Duration,
    universe_load: Duration,
}

/// Observe the world, plan against the previous commit, and ingest the
/// changed files - the whole pipeline `commit` and `status` share.
///
/// Under `dry_run` blobs are probed instead of stored, so the staged rows
/// are the same numbers a real commit would commit.
async fn prepare<W: WorldTree, S: SekaiStore>(
    store: &S,
    world: &W,
    options: &CommitOptions,
    scope: &Scope,
    progress: impl Fn(CommitProgress) + Send,
    dry_run: bool,
    locks: &Arc<OperationLocks<S, W::Lock>>,
) -> Result<Prepared, AppError>
where
    AppError: From<W::Error>,
{
    // Whole-world observation walks and opens files: clone the handle and
    // run it on the blocking pool, owning a share of the locks like every
    // blocking pass.
    let world_handle = world.clone();
    let observe_locks = Arc::clone(locks);
    let observed = tokio::task::spawn_blocking(move || {
        let _locks = observe_locks;
        observe(&world_handle)
    })
    .await??;
    let discover = observed.discover;
    let fingerprint = observed.fingerprint;

    let universe_started = Instant::now();
    let observations: Vec<Observation> = observed.files.iter().map(|(_, obs)| *obs).collect();
    let (previous, plan) =
        sekai_core::usecase::commit::plan_commit(store.meta(), &observations, scope).await?;
    let universe_load = universe_started.elapsed();

    let mut files: HashMap<RegionKey, Changed<W::Region>> =
        HashMap::with_capacity(observed.files.len());
    for (region, observation) in observed.files {
        files.insert(observation.key, (region, observation.fingerprint));
    }
    // Changed files in key order; carries need no file access.
    let changed: Vec<Changed<W::Region>> = plan
        .ingest
        .iter()
        .filter_map(|key| files.remove(key))
        .collect();

    let (ingested, walk) = ingest_changed(
        changed,
        store.cas().clone(),
        options,
        scope.clone(),
        progress,
        dry_run,
        locks,
    )
    .await?;
    let staged = sekai_core::usecase::commit::assemble(plan, &previous, ingested, scope);

    Ok(Prepared {
        previous,
        staged,
        walk,
        discover,
        fingerprint,
        universe_load,
    })
}

impl<W: WorldTree, S: SekaiStore> WorldHandle<'_, W, S>
where
    AppError: From<W::Error>,
{
    /// Preview what a commit would record, without recording anything: no
    /// CAS puts, no metadata commit, no region writes. Only lock files may
    /// be created (ADR-0014).
    ///
    /// Only `scope` is previewed. `progress` fires as changed files
    /// complete. Staged rows are assembled exactly as commit would, then
    /// counted instead of committed - so counts match a subsequent commit
    /// unless the world changes in between.
    pub async fn status(
        &self,
        options: StatusOptions,
        scope: Scope,
        progress: impl Fn(CommitProgress) + Send,
    ) -> Result<(StatusReport, StatusTimings), AppError> {
        let locks = OperationLocks::acquire(
            self.instance,
            StoreLockMode::Shared,
            self.world,
            WorldLockMode::Shared,
        )?;
        status_impl(
            &self.instance.store,
            self.world,
            options,
            scope,
            progress,
            &locks,
        )
        .await
    }
}

impl<W: WorldTree, S: SekaiStore> WorldHandleMut<'_, W, S>
where
    AppError: From<W::Error>,
{
    /// Preview what a commit would record; see [`WorldHandle::status`].
    pub async fn status(
        &self,
        options: StatusOptions,
        scope: Scope,
        progress: impl Fn(CommitProgress) + Send,
    ) -> Result<(StatusReport, StatusTimings), AppError> {
        self.view().status(options, scope, progress).await
    }
}

/// Preview implementation shared by read-only and read-write handles.
async fn status_impl<W: WorldTree, S: SekaiStore>(
    store: &S,
    world: &W,
    options: StatusOptions,
    scope: Scope,
    progress: impl Fn(CommitProgress) + Send,
    locks: &Arc<OperationLocks<S, W::Lock>>,
) -> Result<(StatusReport, StatusTimings), AppError>
where
    AppError: From<W::Error>,
{
    let total = Instant::now();

    // Diff views are preview-irrelevant: always skip the decode work.
    let preview = CommitOptions {
        concurrency: options.concurrency,
        ..CommitOptions::default()
    };
    let Prepared {
        previous,
        staged,
        walk,
        discover,
        fingerprint,
        universe_load,
    } = prepare(store, world, &preview, &scope, progress, true, locks).await?;

    let new_chunks = staged.entries.iter().filter(|e| e.blob.is_some()).count();
    let changed_regions: BTreeSet<RegionKey> = staged
        .entries
        .iter()
        .map(|entry| RegionKey::of(entry.coord))
        .collect();
    let report = StatusReport {
        clean: staged.entries.is_empty() && staged.removed.is_empty() && staged.new_files == 0,
        latest: previous.commit.map(|commit| commit.id),
        changed_regions: changed_regions.len(),
        new_files: staged.new_files,
        deleted_files: staged.removed.len(),
        new_chunks,
        tombstones: staged.tombstones,
        new_blobs: staged.new_blobs,
    };
    let timings = StatusTimings {
        total: total.elapsed(),
        discover,
        fingerprint,
        universe_load,
        region_open: walk.region_open,
        ingest: walk.ingest,
        hash: walk.hash,
    };
    Ok((report, timings))
}

// Whole-world observation: discovery plus fresh fingerprints.
struct Observed<T> {
    /// Every discovered file with its fresh fingerprint.
    files: Vec<(T, Observation)>,
    discover: Duration,
    fingerprint: Duration,
}

/// Discover every region file and fingerprint it. Blocking: file walks and
/// opens belong on a blocking pool, never on an async worker.
fn observe<T: WorldTree>(world: &T) -> Result<Observed<T::Region>, AppError>
where
    AppError: From<T::Error>,
{
    let discover_started = Instant::now();
    let regions = discover_regions(world)?;
    let discover = discover_started.elapsed();

    let fingerprint_started = Instant::now();
    let mut files = Vec::with_capacity(regions.len());
    for (key, region) in regions {
        let observation = Observation {
            key,
            fingerprint: region.fingerprint(key)?,
        };
        files.push((region, observation));
    }
    Ok(Observed {
        files,
        discover,
        fingerprint: fingerprint_started.elapsed(),
    })
}

/// Staged result of walking all discovered regions.
#[derive(Default)]
struct RegionWalk {
    /// Fresh history rows (ingested chunks; tombstones join at assembly).
    entries: Vec<CommitEntry>,
    /// Coordinates present on disk.
    present: HashSet<ChunkCoord>,
    /// Blobs newly written to CAS.
    new_blobs: usize,
    /// Time spent opening changed region files.
    region_open: Duration,
    /// Time spent visiting, hashing, and storing changed chunks.
    ingest: Duration,
    /// Subset of `ingest` spent hashing.
    hash: Duration,
    /// Subset of `ingest` spent in `CAS put`.
    cas: Duration,
    /// Per-region details, sorted by region identity.
    timings: Vec<(RegionKey, RegionTiming)>,
}

impl RegionWalk {
    /// Split into what `core` assembles from and what the report needs.
    fn into_parts(self) -> (Ingested, IngestTimings) {
        let ingested = Ingested {
            entries: self.entries,
            present: self.present.into_iter().collect(),
            new_blobs: self.new_blobs,
        };
        let timings = IngestTimings {
            region_open: self.region_open,
            ingest: self.ingest,
            hash: self.hash,
            cas: self.cas,
            regions: self.timings.into_iter().map(|(_, timing)| timing).collect(),
        };
        (ingested, timings)
    }
}

/// Per-phase ingest timings, reported under `--timing`.
struct IngestTimings {
    /// Time spent opening changed region files.
    region_open: Duration,
    /// Time spent visiting, hashing, and storing changed chunks.
    ingest: Duration,
    /// Subset of `ingest` spent hashing.
    hash: Duration,
    /// Subset of `ingest` spent in `CAS put`.
    cas: Duration,
    /// Per-region details, sorted by region identity.
    regions: Vec<RegionTiming>,
}

/// Ingest changed files, sequentially or across worker threads.
///
/// One file never justifies task overhead, so it stays in a single blocking
/// task with identical counts. Larger sets split into size-balanced groups;
/// each worker owns a private blob handle over the same store (disjoint blob
/// files, unique temp names) and syncs its own shards before returning, so
/// joining orders every durability barrier ahead of the metadata commit. The
/// commit itself stays a single transaction; row merge order across workers
/// is completion-order (DB-irrelevant, one txn).
/// Each worker keeps `locks` alive until it returns, so a cancelled commit
/// cannot hand the store to `gc` while blobs are still being written.
async fn ingest_changed<R: RegionFile, S: SekaiStore, L: WorldLock>(
    changed: Vec<Changed<R>>,
    cas: S::Blobs,
    options: &CommitOptions,
    scope: Scope,
    progress: impl Fn(CommitProgress) + Send,
    dry_run: bool,
    locks: &Arc<OperationLocks<S, L>>,
) -> Result<(Ingested, IngestTimings), AppError>
where
    AppError: From<R::Error>,
{
    let mut walk = RegionWalk {
        timings: Vec::with_capacity(changed.len()),
        ..Default::default()
    };
    if changed.is_empty() {
        return Ok(walk.into_parts());
    }
    let workers = match options.concurrency {
        0 => std::thread::available_parallelism().map_or(4, NonZeroUsize::get),
        n => n,
    }
    .min(changed.len())
    .max(1);
    let groups = partition_groups(changed, workers);
    let files_total: usize = groups.iter().map(Vec::len).sum();
    let mut files_done = 0usize;
    let mut chunks_done = 0usize;

    let mut set = tokio::task::JoinSet::new();
    for group in groups {
        let cas = cas.clone();
        let with_diff = options.with_diff;
        let ignore_tags = options.ignore_tags.clone();
        let scope = scope.clone();
        let locks = Arc::clone(locks);
        set.spawn_blocking(move || {
            let _locks = locks;
            ingest_group(
                group,
                cas,
                with_diff,
                ignore_tags.as_deref(),
                &scope,
                dry_run,
            )
        });
    }
    while let Some(outcome) = set.join_next().await {
        let (outcomes, sync_elapsed) = outcome??;
        walk.cas += sync_elapsed;
        for outcome in outcomes {
            files_done += 1;
            chunks_done += outcome.timing.chunks;
            progress(CommitProgress {
                files_done,
                files_total,
                chunks_done,
            });
            merge_outcome(&mut walk, outcome);
        }
    }
    // Completion order is nondeterministic; keep timing output stable.
    // The identity is the stable key, not the file's location text: a
    // tree spells its locations as it sees fit.
    walk.timings.sort_by_key(|(key, _)| *key);
    Ok(walk.into_parts())
}

/// Split changed files into size-balanced groups, one per worker.
///
/// Largest files first onto the currently lightest group, so every group
/// is non-empty.
fn partition_groups<R>(mut changed: Vec<Changed<R>>, workers: usize) -> Vec<Vec<Changed<R>>> {
    changed.sort_by_key(|item| std::cmp::Reverse(item.1.size));
    let mut groups: Vec<Vec<Changed<R>>> = Vec::with_capacity(workers);
    groups.resize_with(workers, Vec::new);
    let mut loads = vec![0u64; workers];
    for item in changed {
        let lightest = loads
            .iter()
            .enumerate()
            .min_by_key(|(_, load)| *load)
            .map_or(0, |(index, _)| index);
        loads[lightest] += item.1.size;
        groups[lightest].push(item);
    }
    groups.retain(|group| !group.is_empty());
    groups
}

/// Ingest one worker's group with a private blob handle, then sync its shards.
///
/// Returns the per-file outcomes plus the shard-sync time so the caller can
/// account it under `cas` (the durability bucket).
fn ingest_group<R: RegionFile, C: BlockingBlobStore>(
    group: Vec<Changed<R>>,
    mut cas: C,
    with_diff: bool,
    ignore_tags: Option<&[String]>,
    scope: &Scope,
    dry_run: bool,
) -> Result<(Vec<FileOutcome>, Duration), AppError>
where
    AppError: From<R::Error>,
{
    let mut outcomes = Vec::with_capacity(group.len());
    for (region, observed) in group {
        let outcome = ingest_file(
            &region,
            observed,
            &mut cas,
            with_diff,
            ignore_tags,
            scope,
            dry_run,
        )?;
        outcomes.push(outcome);
    }
    if dry_run {
        // Nothing was written: no shards to sync.
        return Ok((outcomes, Duration::ZERO));
    }
    // Barrier for this worker's shards; the join orders all barriers
    // before the metadata commit.
    let sync_started = Instant::now();
    cas.sync_dirs()?;
    Ok((outcomes, sync_started.elapsed()))
}

fn merge_outcome(walk: &mut RegionWalk, outcome: FileOutcome) {
    walk.entries.extend(outcome.entries);
    walk.present.extend(outcome.present);
    walk.new_blobs += outcome.new_blobs;
    walk.region_open += outcome.timing.open;
    walk.ingest += outcome.timing.ingest;
    walk.hash += outcome.timing.hash;
    walk.cas += outcome.timing.cas;
    walk.timings.push((outcome.fingerprint.key, outcome.timing));
}

struct ChunkIngestCtx<'a, C> {
    key: RegionKey,
    scope: &'a Scope,
    cas: &'a mut C,
    scratch: &'a mut Vec<u8>,
    with_diff: bool,
    dry_run: bool,
    ignore: &'a [&'a str],
    entries: &'a mut Vec<CommitEntry>,
    present: &'a mut HashSet<ChunkCoord>,
    new_blobs: &'a mut usize,
}

/// Read, hash, and store one changed region file, staging its rows.
/// Chunks outside `scope` are skipped before hashing: their history stays
/// untouched and fallback keeps resolving them. Under `dry_run` blobs are
/// probed, never stored.
///
/// The fingerprint names the file: discovery observed it under that
/// identity, and the timing row reports the same one.
fn ingest_file<R: RegionFile, C: BlockingBlobStore>(
    region: &R,
    observed: RegionFingerprint,
    cas: &mut C,
    with_diff: bool,
    ignore_tags: Option<&[String]>,
    scope: &Scope,
    dry_run: bool,
) -> Result<FileOutcome, AppError>
where
    AppError: From<R::Error>,
{
    let key = observed.key;
    let opened = Instant::now();
    let bytes = region.read()?;
    let open_elapsed = opened.elapsed();
    let file_bytes = bytes.len() as u64;
    let image = sekai_anvil::RegionImage::from_bytes(bytes, key.rx, key.rz).map_err(|source| {
        AppError::RegionFailed {
            location: region.to_string(),
            source,
        }
    })?;

    let mut ignore_vec = Vec::new();
    let ignore: &[&str] = ignore_tags.map_or(sekai_nbt::DEFAULT_IGNORED, |tags| {
        ignore_vec = tags.iter().map(String::as_str).collect();
        &ignore_vec
    });

    let mut scratch = Vec::new();
    let mut entries = Vec::new();
    let mut present = HashSet::new();
    let mut new_blobs = 0usize;
    let mut failure: Option<AppError> = None;
    let mut chunks = 0usize;
    let mut hash_elapsed = Duration::ZERO;
    let mut cas_elapsed = Duration::ZERO;
    let ingested = Instant::now();
    image
        .visit_chunks(|chunk| {
            if failure.is_some() {
                return false;
            }
            let mut ctx = ChunkIngestCtx {
                key,
                scope,
                cas,
                scratch: &mut scratch,
                with_diff,
                dry_run,
                ignore,
                entries: &mut entries,
                present: &mut present,
                new_blobs: &mut new_blobs,
            };
            match ingest_timed(chunk, &mut ctx) {
                Ok((hash_dt, cas_dt)) => {
                    hash_elapsed += hash_dt;
                    cas_elapsed += cas_dt;
                    chunks += 1;
                }
                Err(err) => {
                    failure = Some(err);
                    return false;
                }
            }
            true
        })
        .map_err(|source| AppError::RegionFailed {
            location: region.to_string(),
            source,
        })?;
    let ingest_elapsed = ingested.elapsed();
    if let Some(err) = failure {
        return Err(err);
    }
    Ok(FileOutcome {
        fingerprint: observed,
        entries,
        present,
        new_blobs,
        timing: RegionTiming {
            location: region.to_string(),
            bytes: file_bytes,
            chunks,
            open: open_elapsed,
            ingest: ingest_elapsed,
            hash: hash_elapsed,
            cas: cas_elapsed,
        },
    })
}

/// Hash one raw chunk, flush it to CAS, and stage its history row.
///
/// Returns the time spent hashing (plus diffing when enabled) and the time
/// spent in `CAS put` separately so digest and exists-check/write costs
/// stay visible in [`CommitTimings`].
fn ingest_timed<C: BlockingBlobStore>(
    chunk: sekai_anvil::Chunk<'_>,
    ctx: &mut ChunkIngestCtx<'_, C>,
) -> Result<(Duration, Duration), AppError> {
    let coord = ChunkCoord::new(ctx.key.dim, ctx.key.kind, chunk.x, chunk.z);
    if !ctx.scope.contains(coord) {
        return Ok((Duration::ZERO, Duration::ZERO));
    }
    let hash_started = Instant::now();
    let hash = sekai_core::hash_blob(chunk.payload);
    // Diff failures degrade to "not computed": the blob itself is intact
    // and the diff column is only a cache.
    let diff = ctx.with_diff.then(|| {
        sekai_anvil::decompress_into(chunk.payload, ctx.scratch)
            .ok()
            .and_then(|_| sekai_core::diff_hash(ctx.scratch, ctx.ignore).ok())
    });
    let hash_elapsed = hash_started.elapsed();
    let cas_started = Instant::now();
    if ctx.dry_run {
        if !ctx.cas.contains_blob(&hash)? {
            *ctx.new_blobs += 1;
        }
    } else if ctx.cas.put_blob(&hash, chunk.payload)? {
        *ctx.new_blobs += 1;
    }
    let cas_elapsed = cas_started.elapsed();
    ctx.entries
        .push(CommitEntry::new(coord, Some(hash), diff.flatten()));
    ctx.present.insert(coord);
    Ok((hash_elapsed, cas_elapsed))
}
