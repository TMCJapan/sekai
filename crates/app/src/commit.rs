//! Recording a world state as a commit: discover files, ingest, filter the
//! delta.
//!
//! Rationale: this is the orchestration half of commit. All policy lives in
//! [`core`](sekai_core::usecase::commit): the previous-state plan, per-chunk
//! staging, delta filtering, tombstone assembly, and the atomic commit.
//! What stays here is mechanics `core` cannot own without breaking its
//! `no_std` boundary: world discovery (filesystem), concurrent blob
//! ingest, the wall clock, and timing collection for `--timing` output.
//!
//! Ingest is fully async: region reads run on the blocking pool (the
//! [`WorldTree`] boundary stays synchronous), and every blob put is an
//! async CAS call. The CAS is append-only, so concurrent workers need no
//! lock at all.
//!
//! Crash order mirrors the core contract: every blob is flushed to CAS
//! *before* the single metadata batch commits, so a torn commit leaves at
//! most orphan blobs (reclaimed by future GC), never dangling references.

use std::collections::{BTreeSet, HashSet};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sekai_core::usecase::commit::Ingested;
use sekai_core::{
    Assembled, BlobStore, ChunkCoord, CommitEntry, CommitId, CommitReport, Previous, RegionKey,
    Scope,
};
use sekai_storage::{SekaiStore, StorageError};
use sekai_world::{Region, RegionFile, WorldLock, WorldLockMode, WorldTree};
use tokio::task::JoinSet;

use crate::error::AppError;
use crate::instance::{SharedWorldGuard, WorldHandle, WorldHandleMut};
use crate::progress::{ProgressBuilder, Reporter};
use crate::regions::discover_regions;
use crate::time::now_ms;

/// Commit behavior knobs.
#[derive(Debug, Clone, Default)]
pub struct CommitOptions {
    /// How many files ingest at once. `0` means one per CPU, `1` stays
    /// sequential.
    pub concurrency: usize,
}

/// Preview behavior knobs.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatusOptions {
    /// How many files ingest at once. `0` means one per CPU, `1` stays
    /// sequential.
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
    /// World discovery, plus sizing the files to ingest.
    pub discover: Duration,
    /// Previous-state load plus ingest planning.
    pub universe_load: Duration,
    /// Opening changed region files.
    pub region_open: Duration,
    /// Visiting and hashing changed chunks (never stored).
    pub ingest: Duration,
    /// Subset of `ingest` spent hashing.
    pub hash: Duration,
}

/// Per-phase timings. Informational only; never changes commit semantics.
#[derive(Debug, Clone, Default)]
pub struct CommitTimings {
    /// Wall-clock total.
    pub total: Duration,
    /// World discovery, plus sizing the files to ingest.
    pub discover: Duration,
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

/// One ingested file's staged rows, ready to merge into the walk.
struct FileOutcome {
    /// The region the file holds.
    key: RegionKey,
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
    /// `progress` fires as changed files complete, possibly before their
    /// blobs are synced: a full count does not mean the commit is durable
    /// yet. The report is identical with or without timings.
    pub async fn commit(
        &mut self,
        options: CommitOptions,
        scope: Scope,
        progress: &impl ProgressBuilder,
    ) -> Result<(CommitReport, CommitTimings), AppError> {
        let total = Instant::now();
        // The world stays stable for the whole read pass; the store needs no
        // lock - blobs are append-only and the metadata write is atomic.
        let guard = SharedWorldGuard::acquire(self.world, WorldLockMode::Shared)?;
        let store = &mut self.instance.store;

        let Prepared {
            previous,
            staged,
            walk,
            discover,
            universe_load,
        } = prepare(store, self.world, &options, &scope, progress, false, &guard).await?;

        let created_at_ms = now_ms()?;
        // Persist blob writes before the metadata commit: every row already
        // committed when its `put` returned, and this single checkpoint folds
        // the WAL back into `blob.sqlite`. Timed under `cas`, the durability
        // bucket. This is the only barrier: ingest leaves it to the call
        // site, never hidden inside adapters.
        let sync_started = Instant::now();
        store.blob_mut().sync().await?;
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
            universe_load,
            region_open: walk.region_open,
            ingest: walk.ingest,
            hash: walk.hash,
            cas_put: walk.cas + dir_sync,
            db_apply,
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
    universe_load: Duration,
}

/// Discover the world, plan against the previous commit, and ingest the
/// changed files - the whole pipeline `commit` and `status` share.
///
/// Under `dry_run` blobs are probed instead of stored, so the staged rows
/// are the same numbers a real commit would commit. The reporter is built
/// from `progress` once the number of files to ingest is known.
async fn prepare<W: WorldTree, S: SekaiStore>(
    store: &S,
    world: &W,
    options: &CommitOptions,
    scope: &Scope,
    progress: &impl ProgressBuilder,
    dry_run: bool,
    guard: &Arc<SharedWorldGuard<W::Lock>>,
) -> Result<Prepared, AppError>
where
    AppError: From<W::Error>,
{
    // Discovery walks directories on the blocking pool, owning a share of
    // the world lock like every file pass.
    let discover_started = Instant::now();
    let world_handle = world.clone();
    let discover_guard = Arc::clone(guard);
    let mut regions = tokio::task::spawn_blocking(move || {
        let _guard = discover_guard;
        discover_regions(&world_handle)
    })
    .await??;
    let discover = discover_started.elapsed();

    let universe_started = Instant::now();
    let keys: Vec<RegionKey> = regions.keys().copied().collect();
    let (previous, plan) =
        sekai_core::usecase::commit::plan_commit(store.meta(), &keys, scope).await?;
    let universe_load = universe_started.elapsed();

    let changed: Vec<(RegionKey, W::Region)> = plan
        .ingest
        .iter()
        .filter_map(|key| regions.remove_entry(key))
        .collect();

    let reporter = progress.start(changed.len());
    let (ingested, walk) = ingest_changed::<W::Region, S, W::Lock>(
        changed,
        store.blob().clone(),
        options,
        scope.clone(),
        reporter,
        dry_run,
        guard,
    )
    .await?;
    let staged = sekai_core::usecase::commit::assemble(plan, &previous, ingested, scope);

    Ok(Prepared {
        previous,
        staged,
        walk,
        discover,
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
    /// Only `scope` is previewed. `progress` reports as for
    /// [`WorldHandleMut::commit`]. Staged rows are assembled exactly as
    /// commit would, then counted instead of committed - so counts match a
    /// subsequent commit unless the world changes in between.
    pub async fn status(
        &self,
        options: StatusOptions,
        scope: Scope,
        progress: &impl ProgressBuilder,
    ) -> Result<(StatusReport, StatusTimings), AppError> {
        let guard = SharedWorldGuard::acquire(self.world, WorldLockMode::Shared)?;
        status_impl(
            &self.instance.store,
            self.world,
            options,
            scope,
            progress,
            &guard,
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
        progress: &impl ProgressBuilder,
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
    progress: &impl ProgressBuilder,
    guard: &Arc<SharedWorldGuard<W::Lock>>,
) -> Result<(StatusReport, StatusTimings), AppError>
where
    AppError: From<W::Error>,
{
    let total = Instant::now();

    // The preview shares the ingest plan but records nothing.
    let preview = CommitOptions {
        concurrency: options.concurrency,
    };
    let Prepared {
        previous,
        staged,
        walk,
        discover,
        universe_load,
    } = prepare(store, world, &preview, &scope, progress, true, guard).await?;

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
        universe_load,
        region_open: walk.region_open,
        ingest: walk.ingest,
        hash: walk.hash,
    };
    Ok((report, timings))
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

/// Ingest changed files concurrently, at most `options.concurrency` at once.
///
/// Each file is its own task, and a finished file frees its slot for the
/// next, so a slow file never holds back the files queued behind it. Tasks
/// clone the blob adapter over the same store and need no lock: the store
/// is append-only, so concurrent puts never conflict. Nothing here syncs
/// the store; the caller places that barrier before the metadata commit.
///
/// `progress` fires per file as its task is joined, before that sync.
async fn ingest_changed<R: RegionFile, S: SekaiStore, L: WorldLock>(
    changed: Vec<(RegionKey, R)>,
    cas: S::Blobs,
    options: &CommitOptions,
    scope: Scope,
    mut progress: impl Reporter,
    dry_run: bool,
    guard: &Arc<SharedWorldGuard<L>>,
) -> Result<(Ingested, IngestTimings), AppError>
where
    AppError: From<R::Error>,
{
    let mut walk = RegionWalk {
        timings: Vec::with_capacity(changed.len()),
        ..Default::default()
    };
    let slots = match options.concurrency {
        0 => std::thread::available_parallelism().map_or(4, NonZeroUsize::get),
        n => n,
    };
    let spawn = |set: &mut JoinSet<Result<FileOutcome, AppError>>,
                 (key, region): (RegionKey, R)| {
        let mut cas = cas.clone();
        let scope = scope.clone();
        let guard = Arc::clone(guard);
        set.spawn(async move { ingest_file(region, key, &mut cas, scope, dry_run, guard).await });
    };

    let mut queued = changed.into_iter();
    let mut set = JoinSet::new();
    for file in queued.by_ref().take(slots) {
        spawn(&mut set, file);
    }
    let mut files_done = 0usize;
    // Completion order surfaces a failed file at once; returning drops the
    // set, which aborts the files still in flight.
    while let Some(joined) = set.join_next().await {
        let outcome = joined.map_err(|source| {
            AppError::Storage(StorageError::backend("ingest worker", source))
        })??;
        files_done += 1;
        progress.step(files_done);
        merge_outcome(&mut walk, outcome);
        if let Some(file) = queued.next() {
            spawn(&mut set, file);
        }
    }
    // Completion order is nondeterministic; keep timing output stable.
    // The identity is the stable key, not the file's location text: a
    // tree spells its locations as it sees fit.
    walk.timings.sort_by_key(|(key, _)| *key);
    Ok(walk.into_parts())
}

fn merge_outcome(walk: &mut RegionWalk, outcome: FileOutcome) {
    walk.entries.extend(outcome.entries);
    walk.present.extend(outcome.present);
    walk.new_blobs += outcome.new_blobs;
    walk.region_open += outcome.timing.open;
    walk.ingest += outcome.timing.ingest;
    walk.hash += outcome.timing.hash;
    walk.cas += outcome.timing.cas;
    walk.timings.push((outcome.key, outcome.timing));
}

/// Read, hash, and store one changed region file, staging its rows.
/// Chunks outside `scope` are skipped before hashing: their history stays
/// untouched and fallback keeps resolving them. Under `dry_run` blobs are
/// probed, never stored.
///
/// `key` names the file: discovery found it under that identity, and the
/// timing row reports the same one.
///
/// Reading and hashing touch the world and run on the blocking pool, which
/// holds `guard` until they end (ADR-0022): an abandoned ingest cannot free
/// the world while a file is still being read. The store pass then awaits
/// the CAS for each payload. Splitting the passes this way keeps chunk
/// bytes from outliving the image that owns them.
async fn ingest_file<R: RegionFile, C: BlobStore<Error = StorageError>, L: WorldLock>(
    region: R,
    key: RegionKey,
    cas: &mut C,
    scope: Scope,
    dry_run: bool,
    guard: Arc<SharedWorldGuard<L>>,
) -> Result<FileOutcome, AppError>
where
    AppError: From<R::Error>,
{
    let location = region.to_string();
    let staged = tokio::task::spawn_blocking(move || {
        let _guard = guard;
        stage_file(&region, key, &scope)
    })
    .await
    .map_err(|source| AppError::Storage(StorageError::backend("ingest worker", source)))??;

    let mut new_blobs = 0usize;
    let mut cas_elapsed = Duration::ZERO;
    // Store pass: every staged payload reaches the CAS. Stops at the first
    // failure; the rows it staged are discarded with the error.
    for write in staged.writes {
        let started = Instant::now();
        let stored = if dry_run {
            cas.contains(&write.hash).await.map(|exists| !exists)
        } else {
            cas.put(&write.hash, &write.payload).await
        };
        cas_elapsed += started.elapsed();
        new_blobs += usize::from(stored?);
    }
    Ok(FileOutcome {
        key,
        entries: staged.entries,
        present: staged.present,
        new_blobs,
        timing: RegionTiming {
            location,
            chunks: staged.chunks,
            open: staged.open,
            ingest: staged.open + staged.hash + cas_elapsed,
            hash: staged.hash,
            cas: cas_elapsed,
        },
    })
}

/// Rows staged by the visit pass, plus what it cost. Payloads are owned so
/// they outlive the image they were read from and can cross to the CAS.
#[derive(Default)]
struct Staged {
    writes: Vec<ChunkWrite>,
    entries: Vec<CommitEntry>,
    present: HashSet<ChunkCoord>,
    chunks: usize,
    open: Duration,
    hash: Duration,
}

/// One chunk payload waiting for its CAS write.
struct ChunkWrite {
    hash: sekai_core::ChunkHash,
    payload: Vec<u8>,
}

/// Open the region file, then hash and stage every in-scope chunk.
fn stage_file<R: RegionFile>(region: &R, key: RegionKey, scope: &Scope) -> Result<Staged, AppError>
where
    AppError: From<R::Error>,
{
    let opened = Instant::now();
    let image: R::RegionImage = region.read(key)?;
    let mut staged = Staged {
        open: opened.elapsed(),
        ..Default::default()
    };
    let mut failure = None;
    image.visit_chunks(|chunk| match stage_chunk(chunk, key, scope, &mut staged) {
        Ok(keep_going) => keep_going,
        Err(error) => {
            failure = Some(error);
            false
        }
    })?;
    failure.map_or(Ok(staged), Err)
}

/// Hash one raw chunk and stage its history row.
///
/// The key is the chunk's normalized digest, so a server rewriting a
/// volatile tag does not produce a new blob or history row. A payload with
/// no normalized view (an unknown codec, a corrupt or non-chunk root) has no
/// key: it fails the file instead of being stored under its raw bytes.
fn stage_chunk(
    chunk: sekai_anvil::Chunk<'_>,
    key: RegionKey,
    scope: &Scope,
    staged: &mut Staged,
) -> Result<bool, AppError> {
    let coord = ChunkCoord::new(key.dim, key.kind, chunk.x, chunk.z);
    if !scope.contains(coord) {
        return Ok(true);
    }
    let hash_started = Instant::now();
    let hash = chunk.normalized_hash().map_err(|source| match source {
        sekai_anvil::ChunkHashError::Decompress(source) => AppError::Anvil(source),
        source => AppError::UnhashableChunk {
            coord,
            rx: key.rx,
            rz: key.rz,
            source,
        },
    })?;
    staged.hash += hash_started.elapsed();
    staged.entries.push(CommitEntry::new(coord, Some(hash)));
    staged.present.insert(coord);
    staged.writes.push(ChunkWrite {
        hash,
        payload: chunk.payload.to_vec(),
    });
    staged.chunks += 1;
    Ok(true)
}
