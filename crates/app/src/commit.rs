//! Recording a world state as a commit: discover files, ingest, filter the
//! snapshot changes.
//!
//! Rationale: this is the orchestration half of commit. The pure policy
//! lives in [`core::staging`](sekai_core::staging): per-chunk staging,
//! change filtering, and absent-coordinate assembly. The store-facing
//! steps live here: [`plan_commit`] reads the base snapshot, and
//! [`record_commit`] merges the tree and records the commit atomically.
//! What only this layer can own is the mechanics beyond the store traits:
//! world discovery (filesystem), concurrent blob ingest, the wall clock,
//! and timing collection for `--timing` output.
//!
//! Ingest is split by executor: region reads and hashing run on the
//! instance's [`ThreadRunner`](sekai_util::ThreadRunner) threads (the
//! [`WorldTree`] boundary stays synchronous), and every blob put is an
//! async CAS call on the caller's executor, interleaved with the joins.
//! The CAS is append-only, so the writes need no lock at all.
//!
//! Crash order mirrors the core contract: every blob is flushed to CAS
//! *before* the single metadata batch commits, so a torn commit leaves at
//! most orphan blobs, never dangling references. Commit records history
//! only: the region-image cache is populated by the separate
//! [`WorldHandleMut::update_cache`] pass, which `sekai commit` runs before
//! recording (ADR-0034).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sekai_core::staging::{Assembled, Ingested, Plan, Previous, assemble};
use sekai_core::{ApplyOutcome, ChunkCoord, CommitEntry, CommitId, RegionKey, Scope};
use sekai_storage::{BlobStore, MetaStore, RegionNode, SekaiStore};
use sekai_util::{JobSet as _, ThreadRunner};
use sekai_world::{Region, RegionFile, WorldError, WorldLock, WorldLockMode, WorldTree};

use crate::cache::prove_regions_unchanged;
use crate::error::AppError;
use crate::instance::{WorldGuard, WorldHandle, WorldHandleMut};
use crate::progress::{BuildProgress, Progress};
use crate::regions::discover_regions;
use crate::time::now_ms;
use crate::workers::worker_error;

/// Outcome of one commit run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitReport {
    /// Newly recorded commit ID.
    pub commit: CommitId,
    /// Chunks present on disk in this commit.
    pub chunks: usize,
    /// Blobs newly written to CAS (the rest deduplicated).
    pub new_blobs: usize,
    /// Vanished chunks removed from the snapshot (a diff count; no
    /// tombstone rows are stored).
    pub tombstones: usize,
}

/// Load prior state and list the in-scope regions to ingest.
///
/// The base is the target of the active ref: a new commit continues the
/// history that ref names, not the newest commit in the store. A store
/// without an active ref (or with an unborn one) starts a new history.
///
/// The caller must hold the world's lock (shared or exclusive) from this
/// call through [`record_commit`]: a commit that moves the active ref in
/// between would make the plan's base and the recorded parent disagree.
///
/// Regions outside `scope` are ignored entirely: they are neither ingested
/// nor reported as removed. Callers must quiesce the source while reading;
/// a concurrent rewrite after the read is not detected until a later
/// commit.
///
/// `discovered` lists every region file found on disk. Every in-scope
/// file is ingested - there is no file-level skip - and
/// [`assemble`](sekai_core::staging::assemble) drops chunk facts whose blob
/// already matches the base snapshot.
pub(crate) async fn plan_commit<M: MetaStore>(
    meta: &M,
    discovered: &[RegionKey],
    scope: &Scope,
) -> Result<(Previous, Plan), M::Error> {
    let active = meta.active_ref().await?;
    // `MetaStore::active_ref` rejects a dangling target (the trait's
    // contract), so the lookup below always finds the commit the ref
    // names: a `None` here is an unborn ref.
    let base = match active.as_ref().and_then(|r| r.commit) {
        Some(id) => meta.lookup_commit(id).await?,
        None => None,
    };
    let mut state: BTreeMap<ChunkCoord, CommitEntry> = BTreeMap::new();
    if let Some(base) = base.as_ref() {
        // The trait visits present entries only: absence is the tombstone,
        // so a vanished coordinate never reaches the state map.
        meta.visit_commit_chunks(base.id, |entry| {
            state.insert(entry.coord, *entry);
            true
        })
        .await?;
    }
    let previous = Previous {
        commit: base,
        state,
    };
    let mut plan = Plan {
        ingest: Vec::new(),
        removed: Vec::new(),
        discovered: BTreeSet::new(),
    };
    for &key in discovered {
        if scope.matches_region(key) {
            plan.discovered.insert(key);
            plan.ingest.push(key);
        }
    }
    let mut removed: BTreeSet<RegionKey> = BTreeSet::new();
    for coord in previous.state.keys() {
        let key = RegionKey::of(*coord);
        if scope.matches_region(key) && !plan.discovered.contains(&key) {
            removed.insert(key);
        }
    }
    plan.removed = removed.into_iter().collect();
    Ok((previous, plan))
}

/// Record assembled changes as one commit.
///
/// CAS must be synced first so every referenced blob is durable before the
/// metadata transaction is committed.
///
/// The new commit continues the history it was planned against: its
/// parent is `previous.commit` - the target of the active ref, which is
/// also the base tree its changes were computed against. The active ref moves
/// onto the new commit in the same transaction. Plan and commit must not be
/// interleaved with another commit; the app holds the store's writer
/// lock for the whole run.
pub(crate) async fn record_commit<M: MetaStore>(
    meta: &mut M,
    previous: &Previous,
    staged: &Assembled,
    created_at_ms: u64,
) -> Result<CommitReport, M::Error> {
    let parents: Vec<CommitId> = previous
        .commit
        .as_ref()
        .map_or_default(|commit| vec![commit.id]);
    let outcome: ApplyOutcome = meta
        .apply_commit_incremental(created_at_ms, &parents, &staged.entries)
        .await?;
    Ok(CommitReport {
        commit: outcome.id,
        chunks: staged.chunks,
        new_blobs: staged.new_blobs,
        tombstones: staged.tombstones,
    })
}

/// Commit behavior knobs.
#[derive(Debug, Clone, Default)]
pub struct CommitOptions {}

/// Preview behavior knobs.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatusOptions {}

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
    /// In-scope regions with snapshot changes (changed chunks or absent
    /// coordinates).
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

impl<W: WorldTree<Error = WorldError>, S: SekaiStore, T: ThreadRunner> WorldHandleMut<'_, W, S, T> {
    /// Scan the bound world and record it as a new commit in the store,
    /// additionally returning per-phase timings.
    ///
    /// History only: no region-image cache entry is written. Population is
    /// the separate [`WorldHandleMut::update_cache`] pass, which callers
    /// run before recording when they want a warm cache (ADR-0034).
    ///
    /// The recorded timestamp is read after ingestion, when the metadata
    /// write begins.
    ///
    /// Only `scope` is ingested: out-of-scope subtrees keep their parent
    /// hashes, so no change or absence is recorded outside it.
    /// `progress_builder` counts changed files as they complete, possibly
    /// before their blobs are synced: a full count does not mean the commit
    /// is durable yet. The report is identical with or without timings.
    pub async fn commit(
        &mut self,
        _options: CommitOptions,
        scope: Scope,
        progress_builder: &impl BuildProgress,
    ) -> Result<(CommitReport, CommitTimings), AppError> {
        let total = Instant::now();
        // The world stays stable for the whole read pass; the store needs no
        // lock - blobs are append-only and the metadata write is atomic.
        let guard = WorldGuard::acquire(self.world, WorldLockMode::Shared)?;
        let store = &mut self.instance.store;

        let Prepared {
            previous,
            staged,
            walk,
            discover,
            universe_load,
        } = prepare(
            store,
            &self.instance.runner,
            self.world,
            &scope,
            progress_builder,
            false,
            &guard,
        )
        .await?;

        let created_at_ms = now_ms()?;
        // Persist blob writes before the metadata commit: puts only append
        // to the blob log, and this single flush fsyncs it. Timed under
        // `cas`, the durability bucket. This is the only barrier: ingest
        // leaves it to the call site, never hidden inside adapters.
        let sync_started = Instant::now();
        store.blob_mut().sync().await?;
        let dir_sync = sync_started.elapsed();
        let db_started = Instant::now();
        let report = record_commit(store.meta_mut(), &previous, &staged, created_at_ms).await?;
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
/// are the same numbers a real commit would commit. Files proven unchanged
/// through the region-image cache stage straight from the committed region
/// nodes. `progress_builder` counts the files to ingest.
async fn prepare<W: WorldTree<Error = WorldError>, S: SekaiStore>(
    store: &S,
    runner: &impl ThreadRunner,
    world: &W,
    scope: &Scope,
    progress_builder: &impl BuildProgress,
    dry_run: bool,
    guard: &Arc<WorldGuard<W::Lock>>,
) -> Result<Prepared, AppError> {
    // Discovery walks directories on the runner, owning a share of the
    // world lock like every file pass.
    let discover_started = Instant::now();
    let world_handle = world.clone();
    let discover_guard = Arc::clone(guard);
    let mut regions = runner
        .call(move || {
            let _guard = discover_guard;
            discover_regions(&world_handle)
        })
        .await
        .map_err(worker_error("ingest worker"))??;
    let discover = discover_started.elapsed();

    let universe_started = Instant::now();
    let keys: Vec<RegionKey> = regions.keys().copied().collect();
    let (previous, plan) = plan_commit(store.meta(), &keys, scope).await?;
    // The proof needs the discovered handles, so it runs before the ingest
    // list takes them out of the map.
    let proven = match previous.commit.as_ref().filter(|_| dry_run) {
        Some(base) => {
            prove_regions_unchanged(runner, store, base.id, &regions, &plan.ingest, guard).await
        }
        None => BTreeMap::new(),
    };
    let universe_load = universe_started.elapsed();

    let changed: Vec<(RegionKey, W::Region)> = plan
        .ingest
        .iter()
        .filter_map(|key| regions.remove_entry(key))
        .collect();

    let (ingested, walk) = ingest_changed::<W::Region, S, W::Lock>(
        changed,
        store.blob().clone(),
        runner,
        scope.clone(),
        &proven,
        progress_builder,
        dry_run,
        guard,
    )
    .await?;
    let staged = assemble(plan, &previous, ingested, scope);

    Ok(Prepared {
        previous,
        staged,
        walk,
        discover,
        universe_load,
    })
}

impl<W: WorldTree<Error = WorldError>, S: SekaiStore, T: ThreadRunner> WorldHandle<'_, W, S, T> {
    /// Preview what a commit would record, without recording anything: no
    /// CAS puts, no metadata commit, no region writes. Only lock files may
    /// be created (ADR-0014).
    ///
    /// Only `scope` is previewed. `progress_builder` counts changed files as
    /// they complete. Staged rows are assembled exactly as commit would, then
    /// counted instead of committed - so counts match a subsequent commit
    /// unless the world changes in between.
    pub async fn status(
        &self,
        _options: StatusOptions,
        scope: Scope,
        progress_builder: &impl BuildProgress,
    ) -> Result<(StatusReport, StatusTimings), AppError> {
        let guard = WorldGuard::acquire(self.world, WorldLockMode::Shared)?;
        status_impl(
            &self.instance.store,
            &self.instance.runner,
            self.world,
            scope,
            progress_builder,
            &guard,
        )
        .await
    }
}

impl<W: WorldTree<Error = WorldError>, S: SekaiStore, T: ThreadRunner> WorldHandleMut<'_, W, S, T> {
    /// Preview what a commit would record; see [`WorldHandle::status`].
    pub async fn status(
        &self,
        options: StatusOptions,
        scope: Scope,
        progress_builder: &impl BuildProgress,
    ) -> Result<(StatusReport, StatusTimings), AppError> {
        self.view().status(options, scope, progress_builder).await
    }
}

/// Preview implementation shared by read-only and read-write handles.
async fn status_impl<W: WorldTree<Error = WorldError>, S: SekaiStore>(
    store: &S,
    runner: &impl ThreadRunner,
    world: &W,
    scope: Scope,
    progress_builder: &impl BuildProgress,
    guard: &Arc<WorldGuard<W::Lock>>,
) -> Result<(StatusReport, StatusTimings), AppError> {
    let total = Instant::now();

    // The preview shares the ingest plan but records nothing.
    let Prepared {
        previous,
        staged,
        walk,
        discover,
        universe_load,
        ..
    } = prepare(store, runner, world, &scope, progress_builder, true, guard).await?;

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
    /// Snapshot changes for ingested chunks; absent coordinates join at
    /// assembly.
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

/// Ingest changed files concurrently, at most the runner's parallelism at
/// once.
///
/// Each file's read and hash pass is its own job on the runner, and a
/// finished file frees its slot for the next, so a slow file never holds
/// back the files queued behind it. Every job holds a share of the world
/// lock until it ends (ADR-0022): an abandoned ingest cannot free the
/// world while a file is still being read. The CAS pass stays on the
/// caller's executor, interleaved with the joins: the store is
/// append-only, so the puts need no lock. Nothing here syncs the store;
/// the caller places that barrier before the metadata commit.
///
/// Proven-unchanged files stage straight from their committed region node
/// and step at once; `progress` steps as each remaining file is stored.
// The proof map joins the existing ingest inputs rather than splitting the
// shared commit/status pipeline into two functions.
#[allow(clippy::too_many_arguments)]
async fn ingest_changed<R: RegionFile<Error = WorldError>, S: SekaiStore, L: WorldLock>(
    changed: Vec<(RegionKey, R)>,
    mut cas: S::Blobs,
    runner: &impl ThreadRunner,
    scope: Scope,
    proven: &BTreeMap<RegionKey, RegionNode>,
    progress_builder: &impl BuildProgress,
    dry_run: bool,
    guard: &Arc<WorldGuard<L>>,
) -> Result<(Ingested, IngestTimings), AppError> {
    let mut progress = progress_builder.build_progress(changed.len());
    let mut walk = RegionWalk {
        timings: Vec::with_capacity(changed.len()),
        ..Default::default()
    };
    let mut spawnable = Vec::with_capacity(changed.len());
    for (key, region) in changed {
        if let Some(node) = proven.get(&key) {
            progress.step();
            merge_outcome(&mut walk, proven_outcome(key, &region, node, &scope));
        } else {
            spawnable.push((key, region));
        }
    }
    let stage_guard = Arc::clone(guard);
    let mut jobs = runner.run(spawnable, move |(key, region)| {
        let _guard = Arc::clone(&stage_guard);
        let location = region.to_string();
        (key, location, stage_file(&region, key, &scope))
    });
    // Completion order surfaces a failed file at once; returning drops the
    // set, which stops the files still queued.
    while let Some(joined) = jobs.join_next().await {
        let (key, location, staged) = joined.map_err(worker_error("ingest worker"))?;
        let staged = staged?;
        let mut new_blobs = 0usize;
        let mut cas_elapsed = Duration::ZERO;
        // Store pass: every staged payload reaches the CAS. Stops at the
        // first failure; the rows it staged are discarded with the error.
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
        progress.step();
        merge_outcome(
            &mut walk,
            FileOutcome {
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
            },
        );
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

/// The outcome a proven-unchanged file stages without being read: its
/// chunks are exactly the committed region node's entries, all already
/// stored, so no blob is new and no work is timed. The present coordinates
/// still matter: assembly tombstones any in-scope coordinate they miss.
fn proven_outcome<R: RegionFile<Error = WorldError>>(
    key: RegionKey,
    region: &R,
    node: &RegionNode,
    scope: &Scope,
) -> FileOutcome {
    let mut entries = Vec::new();
    let mut present = HashSet::new();
    for entry in node.entries() {
        let coord = slot_coord(key, entry.cx, entry.cz);
        if !scope.contains(coord) {
            continue;
        }
        entries.push(CommitEntry::new(coord, Some(entry.hash)));
        present.insert(coord);
    }
    let chunks = present.len();
    FileOutcome {
        key,
        entries,
        present,
        new_blobs: 0,
        timing: RegionTiming {
            location: region.to_string(),
            chunks,
            open: Duration::ZERO,
            ingest: Duration::ZERO,
            hash: Duration::ZERO,
            cas: Duration::ZERO,
        },
    }
}

/// The global coordinate of a region node's local slot.
fn slot_coord(key: RegionKey, cx: u8, cz: u8) -> ChunkCoord {
    ChunkCoord::new(
        key.dim,
        key.kind,
        key.rx * 32 + i32::from(cx),
        key.rz * 32 + i32::from(cz),
    )
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
fn stage_file<R: RegionFile<Error = WorldError>>(
    region: &R,
    key: RegionKey,
    scope: &Scope,
) -> Result<Staged, AppError> {
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

/// Hash one raw chunk and stage its snapshot change.
///
/// The key is the chunk's normalized digest, so a server rewriting a
/// volatile tag does not produce a new blob or tree change. A payload with
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

#[cfg(test)]
mod tests {
    use super::*;
    use sekai_core::staging::stage_present;
    use sekai_core::{Area, ChunkHash, Dimension, Rect, Ref, RefName, RegionKind};
    use sekai_storage::testing::{MemMeta, block_on};

    const OVER: Dimension = Dimension::OVERWORLD;
    const NETHER: Dimension = Dimension::NETHER;
    const REGION: RegionKind = RegionKind::Region;

    const fn key(rx: i32, rz: i32) -> RegionKey {
        RegionKey::new(OVER, REGION, rx, rz)
    }

    const fn coord(x: i32, z: i32) -> ChunkCoord {
        ChunkCoord::new(OVER, REGION, x, z)
    }

    const fn nether_key(rx: i32, rz: i32) -> RegionKey {
        RegionKey::new(NETHER, REGION, rx, rz)
    }

    const fn nether_coord(x: i32, z: i32) -> ChunkCoord {
        ChunkCoord::new(NETHER, REGION, x, z)
    }

    /// The store's active ref: new commits hang from it.
    fn main_ref() -> RefName {
        RefName::parse("main").unwrap()
    }

    /// A fake store with one active ref, the way a fresh store looks.
    fn store_with_ref() -> MemMeta {
        let mut meta = MemMeta::default();
        meta.with_active_ref(&main_ref());
        meta
    }

    #[test]
    fn first_commit_ingests_everything() {
        let meta = store_with_ref();
        let discovered = [key(0, 0)];
        let (previous, plan) = block_on(plan_commit(&meta, &discovered, &Scope::World)).unwrap();
        assert!(previous.commit.is_none());
        assert!(previous.state.is_empty());
        assert_eq!(plan.ingest, vec![key(0, 0)]);
        assert_eq!(plan.removed, [] as [RegionKey; 0]);
    }

    #[test]
    fn a_new_commit_continues_the_active_ref() {
        let mut meta = store_with_ref();
        let main = main_ref();
        let discovered = [key(0, 0)];

        // The active ref is unborn: the commit starts a history.
        let (previous, plan) = block_on(plan_commit(&meta, &discovered, &Scope::World)).unwrap();
        assert!(previous.commit.is_none());
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: vec![stage_present(coord(0, 0), ChunkHash::new([1; 32]))],
                present: BTreeSet::from([coord(0, 0)]),
                new_blobs: 1,
            },
            &Scope::World,
        );
        let first = block_on(record_commit(&mut meta, &previous, &staged, 1_000)).unwrap();
        // The ref moved onto the new commit.
        let active = block_on(meta.active_ref()).unwrap().unwrap();
        assert_eq!(active.commit, Some(first.commit));

        // A second ref at the same commit forks the history: the next
        // commit continues the ref that is active.
        let feature = Ref::branch(RefName::parse("feature").unwrap());
        block_on(meta.create_ref(&feature, first.commit, 1_500)).unwrap();
        block_on(meta.activate_ref(&feature)).unwrap();

        let (previous, plan) = block_on(plan_commit(&meta, &discovered, &Scope::World)).unwrap();
        assert_eq!(previous.commit.as_ref().map(|c| c.id), Some(first.commit));
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: vec![stage_present(coord(0, 0), ChunkHash::new([2; 32]))],
                present: BTreeSet::from([coord(0, 0)]),
                new_blobs: 1,
            },
            &Scope::World,
        );
        let second = block_on(record_commit(&mut meta, &previous, &staged, 2_000)).unwrap();
        assert_eq!(
            block_on(meta.commit_parents(second.commit)).unwrap(),
            vec![first.commit]
        );
        // The ref that was left behind keeps naming the first commit.
        let main = block_on(meta.lookup_ref(&Ref::branch(main)))
            .unwrap()
            .unwrap();
        assert_eq!(main.commit, Some(first.commit));
    }

    #[test]
    fn the_recorded_commit_continues_the_previous_one() {
        let mut meta = store_with_ref();
        let discovered = [key(0, 0)];

        // The first commit starts a history: no previous commit, no
        // parent.
        let (previous, plan) = block_on(plan_commit(&meta, &discovered, &Scope::World)).unwrap();
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: vec![stage_present(coord(0, 0), ChunkHash::new([1; 32]))],
                present: BTreeSet::from([coord(0, 0)]),
                new_blobs: 1,
            },
            &Scope::World,
        );
        let first = block_on(record_commit(&mut meta, &previous, &staged, 1_000)).unwrap();
        assert_eq!(block_on(meta.commit_parents(first.commit)).unwrap(), vec![]);

        // The second commit continues the history of the commit it was
        // planned against.
        let (previous, plan) = block_on(plan_commit(&meta, &discovered, &Scope::World)).unwrap();
        assert_eq!(previous.commit.as_ref().map(|c| c.id), Some(first.commit));
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: vec![stage_present(coord(0, 0), ChunkHash::new([2; 32]))],
                present: BTreeSet::from([coord(0, 0)]),
                new_blobs: 1,
            },
            &Scope::World,
        );
        let second = block_on(record_commit(&mut meta, &previous, &staged, 2_000)).unwrap();
        assert_eq!(
            block_on(meta.commit_parents(second.commit)).unwrap(),
            vec![first.commit]
        );
    }

    #[test]
    fn an_empty_region_is_not_a_new_file() {
        let meta = store_with_ref();
        let discovered = [key(0, 0)];
        let (previous, plan) = block_on(plan_commit(&meta, &discovered, &Scope::World)).unwrap();
        let staged = assemble(plan, &previous, Ingested::default(), &Scope::World);
        assert_eq!(staged.entries, [] as [CommitEntry; 0]);
        assert_eq!(staged.new_files, 0);
        assert_eq!(staged.chunks, 0);
    }

    #[test]
    fn unchanged_chunks_write_no_rows() {
        let mut meta = store_with_ref();
        let discovered = [key(0, 0)];
        let entries = [
            stage_present(coord(0, 0), ChunkHash::new([1; 32])),
            stage_present(coord(1, 0), ChunkHash::new([2; 32])),
        ];
        let present = BTreeSet::from([coord(0, 0), coord(1, 0)]);
        let (previous, plan) = block_on(plan_commit(&meta, &discovered, &Scope::World)).unwrap();
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: entries.to_vec(),
                present: present.clone(),
                new_blobs: 2,
            },
            &Scope::World,
        );
        let first = block_on(record_commit(&mut meta, &previous, &staged, 1_000)).unwrap();

        let (previous, plan) = block_on(plan_commit(&meta, &discovered, &Scope::World)).unwrap();
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: entries.to_vec(),
                present,
                new_blobs: 0,
            },
            &Scope::World,
        );
        assert_eq!(staged.entries, [] as [CommitEntry; 0]);
        assert_eq!(staged.chunks, 2);
        let report = block_on(record_commit(&mut meta, &previous, &staged, 2_000)).unwrap();
        assert_ne!(report.commit, first.commit);
        assert_eq!(report.new_blobs, 0);
        assert_eq!(report.tombstones, 0);
        let kept = block_on(meta.lookup_chunk(report.commit, &coord(0, 0)))
            .unwrap()
            .unwrap();
        assert_eq!(kept.blob, Some(ChunkHash::new([1; 32])));
    }

    #[test]
    fn changed_chunk_writes_only_its_row() {
        let mut meta = store_with_ref();
        let discovered = [key(0, 0)];
        let (previous, plan) = block_on(plan_commit(&meta, &discovered, &Scope::World)).unwrap();
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: vec![
                    stage_present(coord(0, 0), ChunkHash::new([1; 32])),
                    stage_present(coord(1, 0), ChunkHash::new([2; 32])),
                ],
                present: BTreeSet::from([coord(0, 0), coord(1, 0)]),
                new_blobs: 2,
            },
            &Scope::World,
        );
        block_on(record_commit(&mut meta, &previous, &staged, 1_000)).unwrap();

        let (previous, plan) = block_on(plan_commit(&meta, &discovered, &Scope::World)).unwrap();
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: vec![
                    stage_present(coord(0, 0), ChunkHash::new([9; 32])),
                    stage_present(coord(1, 0), ChunkHash::new([2; 32])),
                ],
                present: BTreeSet::from([coord(0, 0), coord(1, 0)]),
                new_blobs: 1,
            },
            &Scope::World,
        );
        assert_eq!(
            staged.entries,
            vec![stage_present(coord(0, 0), ChunkHash::new([9; 32]))]
        );
        assert_eq!(staged.chunks, 2);
        let report = block_on(record_commit(&mut meta, &previous, &staged, 2_000)).unwrap();
        assert_eq!(report.chunks, 2);
        assert_eq!(report.new_blobs, 1);
        assert_eq!(report.tombstones, 0);

        let got = block_on(meta.lookup_chunk(report.commit, &coord(0, 0)))
            .unwrap()
            .unwrap();
        assert_eq!(got.blob, Some(ChunkHash::new([9; 32])));
        let kept = block_on(meta.lookup_chunk(report.commit, &coord(1, 0)))
            .unwrap()
            .unwrap();
        assert_eq!(kept.blob, Some(ChunkHash::new([2; 32])));
    }

    #[test]
    fn missing_coordinates_become_tombstones() {
        let mut meta = store_with_ref();
        block_on(meta.apply_commit_incremental(
            1_000,
            &[],
            &[
                stage_present(coord(0, 0), ChunkHash::new([1; 32])),
                stage_present(coord(1, 0), ChunkHash::new([2; 32])),
            ],
        ))
        .unwrap();
        let (previous, plan) = block_on(plan_commit(&meta, &[], &Scope::World)).unwrap();
        assert_eq!(plan.removed, vec![key(0, 0)]);
        let staged = assemble(plan, &previous, Ingested::default(), &Scope::World);
        assert_eq!(staged.chunks, 0);
        assert_eq!(staged.tombstones, 2);

        let report = block_on(record_commit(&mut meta, &previous, &staged, 2_000)).unwrap();
        assert_eq!(report.tombstones, 2);
        let tomb = block_on(meta.lookup_chunk(report.commit, &coord(0, 0))).unwrap();
        assert!(tomb.is_none(), "absence is the tombstone");
    }

    #[test]
    fn already_tombstoned_coordinates_record_no_further_rows() {
        let mut meta = store_with_ref();
        block_on(meta.apply_commit_incremental(
            1_000,
            &[],
            &[
                stage_present(coord(0, 0), ChunkHash::new([1; 32])),
                stage_present(coord(32, 0), ChunkHash::new([2; 32])),
            ],
        ))
        .unwrap();

        // Region (0,0) is gone from disk: its chunk tombstones once while
        // region (1,0) is still present.
        let discovered = [key(1, 0)];
        let (previous, plan) = block_on(plan_commit(&meta, &discovered, &Scope::World)).unwrap();
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: vec![stage_present(coord(32, 0), ChunkHash::new([2; 32]))],
                present: BTreeSet::from([coord(32, 0)]),
                new_blobs: 0,
            },
            &Scope::World,
        );
        assert_eq!(staged.tombstones, 1);
        assert_eq!(staged.chunks, 1);
        let report = block_on(record_commit(&mut meta, &previous, &staged, 2_000)).unwrap();
        assert_eq!(report.tombstones, 1);
        assert_eq!(report.chunks, 1);

        // Repeat commit with the same world: the tombstone already resolves
        // through fallback, so it records no fresh row and the vanished
        // chunk is not carried back as present.
        let (previous, plan) = block_on(plan_commit(&meta, &discovered, &Scope::World)).unwrap();
        assert_eq!(
            previous.state.keys().copied().collect::<Vec<_>>(),
            [coord(32, 0)]
        );
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: vec![stage_present(coord(32, 0), ChunkHash::new([2; 32]))],
                present: BTreeSet::from([coord(32, 0)]),
                new_blobs: 0,
            },
            &Scope::World,
        );
        assert_eq!(staged.entries, [] as [CommitEntry; 0]);
        assert_eq!(staged.tombstones, 0);
        assert_eq!(staged.chunks, 1);

        let report = block_on(record_commit(&mut meta, &previous, &staged, 3_000)).unwrap();
        assert_eq!(report.tombstones, 0);
        assert_eq!(report.chunks, 1);
        assert!(
            block_on(meta.lookup_chunk(report.commit, &coord(0, 0)))
                .unwrap()
                .is_none()
        );
        let kept = block_on(meta.lookup_chunk(report.commit, &coord(32, 0)))
            .unwrap()
            .unwrap();
        assert_eq!(kept.blob, Some(ChunkHash::new([2; 32])));
    }

    #[test]
    fn scoped_plan_ignores_out_of_scope_regions() {
        let mut meta = store_with_ref();
        block_on(meta.apply_commit_incremental(
            1_000,
            &[],
            &[
                stage_present(coord(0, 0), ChunkHash::new([1; 32])),
                stage_present(nether_coord(0, 0), ChunkHash::new([2; 32])),
            ],
        ))
        .unwrap();

        // Only the nether region is discovered (overworld file deleted from
        // disk), but the overworld scope sees neither ingest nor removal.
        let discovered = [nether_key(0, 0)];
        let scope = Scope::dimension(NETHER);
        let (_, plan) = block_on(plan_commit(&meta, &discovered, &scope)).unwrap();
        assert_eq!(plan.ingest, vec![nether_key(0, 0)]);
        assert_eq!(plan.removed, [] as [RegionKey; 0]);

        // Out-of-scope regions never enter the plan at all.
        let discovered = [key(0, 0), nether_key(0, 0)];
        let (_, plan) = block_on(plan_commit(&meta, &discovered, &Scope::World)).unwrap();
        assert_eq!(plan.ingest, vec![key(0, 0), nether_key(0, 0)]);
    }

    #[test]
    fn scoped_assemble_tombstones_only_in_scope() {
        let mut meta = store_with_ref();
        block_on(meta.apply_commit_incremental(
            1_000,
            &[],
            &[
                stage_present(coord(0, 0), ChunkHash::new([1; 32])),
                stage_present(coord(1, 0), ChunkHash::new([2; 32])),
                stage_present(nether_coord(0, 0), ChunkHash::new([3; 32])),
            ],
        ))
        .unwrap();

        // Scoped commit of the overworld re-ingests its changed region:
        // (1,0) is genuinely gone, the nether chunk is out of scope and
        // must not become a tombstone.
        let discovered = [key(0, 0)];
        let scope = Scope::dimension(OVER);
        let (previous, plan) = block_on(plan_commit(&meta, &discovered, &scope)).unwrap();
        assert_eq!(plan.ingest, vec![key(0, 0)]);
        assert_eq!(plan.removed, [] as [RegionKey; 0]);
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: vec![stage_present(coord(0, 0), ChunkHash::new([9; 32]))],
                present: BTreeSet::from([coord(0, 0)]),
                new_blobs: 1,
            },
            &scope,
        );
        assert_eq!(staged.tombstones, 1);
        assert_eq!(staged.chunks, 1);
        let tombstoned: Vec<ChunkCoord> = staged
            .entries
            .iter()
            .filter(|entry| entry.blob.is_none())
            .map(|entry| entry.coord)
            .collect();
        assert_eq!(tombstoned, vec![coord(1, 0)]);

        let report = block_on(record_commit(&mut meta, &previous, &staged, 2_000)).unwrap();
        assert_eq!(report.tombstones, 1);
        // The nether chunk still resolves through fallback, untouched.
        let kept = block_on(meta.lookup_chunk(report.commit, &nether_coord(0, 0)))
            .unwrap()
            .unwrap();
        assert_eq!(kept.blob, Some(ChunkHash::new([3; 32])));
    }

    fn over_rect(rect: Rect) -> Scope {
        Scope::Select {
            kinds: vec![REGION],
            areas: vec![(OVER, Area::Rect(rect))],
        }
    }

    #[test]
    fn partial_scope_then_full_commit_reingests_unread_chunks() {
        let mut meta = store_with_ref();
        block_on(meta.apply_commit_incremental(
            1_000,
            &[],
            &[
                stage_present(coord(0, 0), ChunkHash::new([1; 32])),
                stage_present(coord(1, 0), ChunkHash::new([2; 32])),
            ],
        ))
        .unwrap();

        // Both chunks changed on disk, but the scope reads only (0,0).
        let discovered = [key(0, 0)];
        let scope = over_rect(Rect::new(0, 0, 0, 0));
        let (previous, plan) = block_on(plan_commit(&meta, &discovered, &scope)).unwrap();
        assert_eq!(plan.ingest, vec![key(0, 0)]);
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: vec![stage_present(coord(0, 0), ChunkHash::new([9; 32]))],
                present: BTreeSet::from([coord(0, 0)]),
                new_blobs: 1,
            },
            &scope,
        );
        assert_eq!(staged.tombstones, 0);
        block_on(record_commit(&mut meta, &previous, &staged, 2_000)).unwrap();

        // The next full commit rereads the whole file and records the
        // unread (1,0) change, while (0,0) stays unchanged.
        let (previous, plan) = block_on(plan_commit(&meta, &discovered, &Scope::World)).unwrap();
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: vec![
                    stage_present(coord(0, 0), ChunkHash::new([9; 32])),
                    stage_present(coord(1, 0), ChunkHash::new([7; 32])),
                ],
                present: BTreeSet::from([coord(0, 0), coord(1, 0)]),
                new_blobs: 1,
            },
            &Scope::World,
        );
        assert_eq!(
            staged.entries,
            vec![stage_present(coord(1, 0), ChunkHash::new([7; 32]))]
        );
        let report = block_on(record_commit(&mut meta, &previous, &staged, 3_000)).unwrap();
        let got = block_on(meta.lookup_chunk(report.commit, &coord(1, 0)))
            .unwrap()
            .unwrap();
        assert_eq!(got.blob, Some(ChunkHash::new([7; 32])));
    }
}
