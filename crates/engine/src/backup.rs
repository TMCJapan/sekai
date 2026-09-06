//! Incremental world backup: fingerprint, ingest changes, carry the rest.
//!
//! Rationale: backup never parses NBT - `blob_hash` runs over raw sector
//! bytes, so the hot path is read + hash + CAS write with zero decoding.
//! Change detection (`diff`) is deliberately not computed here; parsing
//! every chunk would triple scan cost for data no consumer reads yet.
//!
//! Unchanged region files skip ingestion entirely: each file carries a
//! `(mtime, size, header hash)` fingerprint in the derived `region_state`
//! table, and a file matching all three signals keeps its previous history
//! rows via one `INSERT ... SELECT` per region instead of a per-chunk loop.
//! Tombstones keep history total without a global chunk census: the known
//! universe is exactly the coordinate set of the latest snapshot, so every
//! snapshot re-records every known coordinate (present, carried, or
//! tombstone) and the induction holds from the first backup on.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sekai_core::{BlobHasher as _, ChunkCoord, MetaStore as _, RawChunk, RegionReader as _};
use sekai_mca::RegionFile;
use sekai_storage::{FileCas, RegionFingerprint, RegionKey, RegionStateEntry, SnapshotEntry};

use crate::discover::{RegionRef, discover};
use crate::error::EngineError;
use crate::fingerprint::fingerprint_file;
use crate::hash::Blake3Hasher;
use crate::store::Store;
use crate::timing::{BackupTimings, RegionTiming};

/// Outcome of one [`backup`] run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupReport {
    /// Newly recorded snapshot ID.
    pub snapshot: sekai_core::SnapshotId,
    /// Chunks present on disk in this backup.
    pub chunks: usize,
    /// Blobs newly written to CAS (the rest deduplicated).
    pub new_blobs: usize,
    /// Tombstone rows recorded for vanished chunks.
    pub tombstones: usize,
    /// Region files skipped via fingerprint match.
    pub skipped_regions: usize,
    /// History rows carried over for skipped regions.
    pub carried_chunks: usize,
}

/// Changed file plus its fresh fingerprint, awaiting ingest.
type Changed<'a> = (&'a RegionRef, RegionFingerprint);

/// One ingested file's staged rows, ready to merge into the walk.
struct FileOutcome {
    /// Fresh fingerprint for `region_state` upsert.
    fingerprint: RegionFingerprint,
    /// History rows for the file's chunks.
    entries: Vec<SnapshotEntry>,
    /// Coordinates present in the file.
    present: HashSet<ChunkCoord>,
    /// Blobs newly written to CAS.
    new_blobs: usize,
    /// Per-region timing slice.
    timing: RegionTiming,
}
/// Previous snapshot plus the state the walk needs.
struct Previous {
    /// Latest snapshot, when the store is non-empty.
    snapshot: Option<sekai_core::Snapshot>,
    /// Coordinates of the latest snapshot (empty on first run).
    universe: HashSet<ChunkCoord>,
    /// Stored fingerprints keyed by `(dim, kind, rx, rz)`.
    states: HashMap<(i32, u8, i32, i32), RegionStateEntry>,
    /// Time spent loading both.
    load: Duration,
}

/// Staged result of walking all discovered regions.
struct RegionWalk {
    /// Fresh history rows (ingested chunks plus tombstones below).
    entries: Vec<SnapshotEntry>,
    /// Coordinates present on disk.
    present: HashSet<ChunkCoord>,
    /// Blobs newly written to CAS.
    new_blobs: usize,
    /// Fingerprint-matched regions, carried from the previous snapshot.
    carries: Vec<RegionKey>,
    /// Fresh fingerprints for ingested files.
    fingerprints: Vec<RegionFingerprint>,
    /// Discovered region identities (for dropping stale state).
    discovered: HashSet<(i32, u8, i32, i32)>,
    /// Time spent fingerprinting files.
    fingerprint: Duration,
    /// Time spent opening changed region files.
    region_open: Duration,
    /// Time spent visiting, hashing, and storing changed chunks.
    ingest: Duration,
    /// Subset of `ingest` spent hashing.
    hash: Duration,
    /// Subset of `ingest` spent in `CAS put`.
    cas: Duration,
    /// Per-region details for ingested files, in discovery order.
    timings: Vec<RegionTiming>,
}

/// Scan `world` and record it as a new snapshot in `store`.
///
/// Crash order: every blob is flushed to CAS *before* the single metadata
/// transaction commits, so a torn backup leaves at most orphan blobs
/// (reclaimed by future GC), never dangling references.
pub fn backup(world: &Path, store: &mut Store) -> Result<BackupReport, EngineError> {
    Ok(backup_with_metrics(world, store)?.0)
}

/// Scan `world` like [`backup`], additionally returning per-phase timings.
///
/// The report is identical to [`backup`]'s; timings are informational only
/// and must never change backup semantics.
pub fn backup_with_metrics(
    world: &Path,
    store: &mut Store,
) -> Result<(BackupReport, BackupTimings), EngineError> {
    let total = Instant::now();
    let previous = load_previous(store)?;

    let discover_started = Instant::now();
    let regions = discover(world)?;
    let discover = discover_started.elapsed();

    let mut walk = walk_regions(&regions, &previous.states, previous.universe.len(), store)?;
    carry_present(&previous.universe, &walk.carries, &mut walk.present);

    let mut tombstones = 0usize;
    for coord in &previous.universe {
        if !walk.present.contains(coord) {
            walk.entries.push(SnapshotEntry::new(*coord, None, None));
            tombstones += 1;
        }
    }

    // State rows for files gone from disk leave with this snapshot.
    let removed: Vec<RegionKey> = previous
        .states
        .values()
        .filter(|st| {
            !walk
                .discovered
                .contains(&(st.key.dim.raw(), st.key.kind.raw(), st.key.rx, st.key.rz))
        })
        .map(|st| st.key)
        .collect();

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))?;
    // Persist batched shard-directory renames before the metadata commit:
    // file data is already fsynced per blob, and this single barrier (at
    // most one fsync per touched shard) is what makes every referenced blob
    // crash-durable. Timed under `cas`, the durability bucket. Pinned to the
    // `BlobStore` seam (not the inherent method) so the call site only
    // depends on the trait.
    let sync_started = Instant::now();
    sekai_core::BlobStore::sync(store.cas_mut())?;
    let dir_sync = sync_started.elapsed();
    let db_started = Instant::now();
    let carry_from = previous
        .snapshot
        .map(|snapshot| (snapshot.id, walk.carries.as_slice()));
    let outcome = store.meta_mut().apply_snapshot_incremental(
        now_ms,
        &walk.entries,
        carry_from,
        &walk.fingerprints,
        &removed,
    )?;
    let db_apply = db_started.elapsed();

    let report = BackupReport {
        snapshot: outcome.id,
        chunks: walk.present.len(),
        new_blobs: walk.new_blobs,
        tombstones,
        skipped_regions: walk.carries.len(),
        carried_chunks: outcome.carried_chunks,
    };
    let timings = BackupTimings {
        total: total.elapsed(),
        discover,
        universe_load: previous.load,
        fingerprint: walk.fingerprint,
        region_open: walk.region_open,
        ingest: walk.ingest,
        hash: walk.hash,
        cas_put: walk.cas + dir_sync,
        cas_checked: walk.present.len(),
        db_apply,
        skipped_regions: walk.carries.len(),
        carried_chunks: outcome.carried_chunks,
        regions: walk.timings,
    };
    Ok((report, timings))
}

/// Load the previous snapshot's coordinates and region fingerprints.
fn load_previous(store: &Store) -> Result<Previous, EngineError> {
    let started = Instant::now();
    let mut universe: HashSet<ChunkCoord> = HashSet::new();
    let snapshot = store.meta().latest_snapshot()?;
    if let Some(latest) = snapshot {
        store.meta().visit_snapshot_chunks(latest.id, |entry| {
            universe.insert(entry.coord);
            true
        })?;
    }
    let mut states: HashMap<(i32, u8, i32, i32), RegionStateEntry> = HashMap::new();
    for state in store.meta().load_region_states()? {
        states.insert(
            (
                state.key.dim.raw(),
                state.key.kind.raw(),
                state.key.rx,
                state.key.rz,
            ),
            state,
        );
    }
    Ok(Previous {
        snapshot,
        universe,
        states,
        load: started.elapsed(),
    })
}

/// Walk every discovered region, ingesting changed files and staging skips.
fn walk_regions(
    regions: &[RegionRef],
    states: &HashMap<(i32, u8, i32, i32), RegionStateEntry>,
    universe_len: usize,
    store: &mut Store,
) -> Result<RegionWalk, EngineError> {
    let mut walk = RegionWalk {
        entries: Vec::with_capacity(universe_len),
        present: HashSet::with_capacity(universe_len),
        new_blobs: 0,
        carries: Vec::new(),
        fingerprints: Vec::new(),
        discovered: HashSet::with_capacity(regions.len()),
        fingerprint: Duration::ZERO,
        region_open: Duration::ZERO,
        ingest: Duration::ZERO,
        hash: Duration::ZERO,
        cas: Duration::ZERO,
        timings: Vec::with_capacity(regions.len()),
    };
    let mut changed: Vec<Changed<'_>> = Vec::new();
    for region in regions {
        let key_tuple = (
            region.dim.raw(),
            region.kind.raw(),
            region.region_x,
            region.region_z,
        );
        walk.discovered.insert(key_tuple);
        let key = RegionKey::new(region.dim, region.kind, region.region_x, region.region_z);

        let fp_started = Instant::now();
        let file_fp = fingerprint_file(&region.path)?;
        walk.fingerprint += fp_started.elapsed();
        let observed = RegionFingerprint {
            key,
            mtime_ms: file_fp.mtime_ms,
            size: file_fp.size,
            header_hash: file_fp.header_hash,
        };
        // Fingerprint and ingest open the file separately, and a carried
        // file is not re-read before the DB commit: a concurrent rewrite
        // after the fingerprint match is silently missed by this snapshot
        // (the next run mismatches and re-ingests). The mismatch path fails
        // safe (stale fingerprint stored, re-ingested next run, or ingest
        // errors). Callers must quiesce the server before snapshotting.
        // Unchanged files skip read/hash/CAS; their rows carry over below.
        if states
            .get(&key_tuple)
            .is_some_and(|st| observed.matches_state(st))
        {
            walk.carries.push(key);
            continue;
        }
        changed.push((region, observed));
    }
    ingest_changed(changed, store, &mut walk)?;
    Ok(walk)
}

/// Ingest changed files, sequentially or across worker threads.
///
/// One file never justifies thread overhead, so it stays on the calling
/// thread with identical counts. Larger sets split into size-balanced
/// groups; each worker owns a private `FileCas` handle over the same root
/// (disjoint blob files, unique temp names) and syncs its own shards before
/// returning, so the scope join orders every durability barrier ahead of the
/// metadata commit. The commit itself stays a single transaction; row merge
/// order across workers is completion-order (DB-irrelevant, one txn).
fn ingest_changed(
    changed: Vec<Changed<'_>>,
    store: &mut Store,
    walk: &mut RegionWalk,
) -> Result<(), EngineError> {
    if changed.len() <= 1 {
        for (region, observed) in changed {
            let mut entries = Vec::new();
            let mut present = HashSet::new();
            let mut new_blobs = 0usize;
            let (fingerprint, timing) = ingest_file(
                region,
                observed,
                store.cas_mut(),
                &mut entries,
                &mut present,
                &mut new_blobs,
            )?;
            merge_outcome(
                walk,
                FileOutcome {
                    fingerprint,
                    entries,
                    present,
                    new_blobs,
                    timing,
                },
            );
        }
        return Ok(());
    }
    // Owned root: workers must not borrow `store` (its SQLite handle is not
    // shareable across threads), only the CAS directory path.
    let cas_root = store.cas().root().to_path_buf();
    let groups = partition_groups(changed);
    let count = groups.len();
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        for group in groups {
            let tx = tx.clone();
            let cas_root = cas_root.clone();
            scope.spawn(move || {
                let outcome = ingest_group(group, &cas_root);
                // The receiver outlives the scope, so this fails only if the
                // worker panicked; library code has no panic paths, and a
                // missing message still surfaces below via the receive count.
                let _ = tx.send(outcome);
            });
        }
    });
    drop(tx);
    for _ in 0..count {
        match rx.recv() {
            Ok(Ok((outcomes, sync_elapsed))) => {
                walk.cas += sync_elapsed;
                for outcome in outcomes {
                    merge_outcome(walk, outcome);
                }
            }
            Ok(Err(err)) => return Err(err),
            Err(_) => {
                return Err(EngineError::Io {
                    path: cas_root,
                    source: std::io::Error::other("cas worker ended without reporting"),
                });
            }
        }
    }
    // Completion order is nondeterministic; keep timing output stable.
    walk.timings.sort_by_key(|timing| timing.path.clone());
    Ok(())
}

/// Split changed files into size-balanced groups, one per worker.
///
/// Largest files first onto the currently lightest group; worker count is
/// hardware parallelism bounded by actual work, so every group is non-empty.
fn partition_groups<'a>(mut changed: Vec<Changed<'a>>) -> Vec<Vec<Changed<'a>>> {
    changed.sort_by_key(|item| std::cmp::Reverse(item.1.size));
    let workers = thread::available_parallelism()
        .map_or(4, NonZeroUsize::get)
        .min(changed.len());
    let mut groups: Vec<Vec<Changed<'a>>> = Vec::with_capacity(workers);
    groups.resize_with(workers, Vec::new);
    let mut loads = vec![0u64; workers];
    for item in changed {
        let mut lightest = 0usize;
        for (index, load) in loads.iter().enumerate() {
            if *load < loads[lightest] {
                lightest = index;
            }
        }
        loads[lightest] += item.1.size;
        groups[lightest].push(item);
    }
    groups.retain(|group| !group.is_empty());
    groups
}

/// Ingest one worker's group with a private CAS handle, then sync its shards.
///
/// Returns the per-file outcomes plus the shard-sync time so the caller can
/// account it under `cas` (the durability bucket).
fn ingest_group(
    group: Vec<Changed<'_>>,
    cas_root: &Path,
) -> Result<(Vec<FileOutcome>, Duration), EngineError> {
    let mut cas = FileCas::open(cas_root)?;
    let mut outcomes = Vec::with_capacity(group.len());
    for (region, observed) in group {
        let mut entries = Vec::new();
        let mut present = HashSet::new();
        let mut new_blobs = 0usize;
        let (fingerprint, timing) = ingest_file(
            region,
            observed,
            &mut cas,
            &mut entries,
            &mut present,
            &mut new_blobs,
        )?;
        outcomes.push(FileOutcome {
            fingerprint,
            entries,
            present,
            new_blobs,
            timing,
        });
    }
    // Barrier for this worker's shards; the scope join orders all barriers
    // before the metadata commit. Pinned to the trait seam.
    let sync_started = Instant::now();
    sekai_core::BlobStore::sync(&mut cas)?;
    Ok((outcomes, sync_started.elapsed()))
}

/// Fold one file's staged rows into the walk.
fn merge_outcome(walk: &mut RegionWalk, outcome: FileOutcome) {
    walk.entries.extend(outcome.entries);
    walk.present.extend(outcome.present);
    walk.new_blobs += outcome.new_blobs;
    walk.region_open += outcome.timing.open;
    walk.ingest += outcome.timing.ingest;
    walk.hash += outcome.timing.hash;
    walk.cas += outcome.timing.cas;
    walk.fingerprints.push(outcome.fingerprint);
    walk.timings.push(outcome.timing);
}

/// Read, hash, and store one changed region file, staging its rows.
///
/// Row staging goes through the out-params so the sequential and worker
/// paths share one shape; the returned fingerprint feeds the `region_state`
/// upsert and the timing slice feeds `--timing`.
fn ingest_file(
    region: &RegionRef,
    observed: RegionFingerprint,
    cas: &mut FileCas,
    entries: &mut Vec<SnapshotEntry>,
    present: &mut HashSet<ChunkCoord>,
    new_blobs: &mut usize,
) -> Result<(RegionFingerprint, RegionTiming), EngineError> {
    let opened = Instant::now();
    let file = RegionFile::open(&region.path, region.dim, region.kind)?;
    let open_elapsed = opened.elapsed();
    let bytes = file.image().len() as u64;

    let mut failure: Option<EngineError> = None;
    let mut chunks = 0usize;
    let mut hash_elapsed = Duration::ZERO;
    let mut cas_elapsed = Duration::ZERO;
    let ingested = Instant::now();
    file.visit_chunks(|chunk| {
        if failure.is_some() {
            return false;
        }
        match ingest_timed(chunk, cas, entries, present, new_blobs) {
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
    })?;
    let ingest_elapsed = ingested.elapsed();
    if let Some(err) = failure {
        return Err(err);
    }
    Ok((
        observed,
        RegionTiming {
            path: region.path.clone(),
            bytes,
            chunks,
            open: open_elapsed,
            ingest: ingest_elapsed,
            hash: hash_elapsed,
            cas: cas_elapsed,
        },
    ))
}

/// Mark previous coordinates under skipped regions as still present.
///
/// The files are byte-identical to the previous snapshot by fingerprint, so
/// their rows carry over verbatim; this in-memory filter replaces per-region
/// database reads.
fn carry_present(
    universe: &HashSet<ChunkCoord>,
    carries: &[RegionKey],
    present: &mut HashSet<ChunkCoord>,
) {
    if carries.is_empty() {
        return;
    }
    let skipped: HashSet<(i32, u8, i32, i32)> = carries
        .iter()
        .map(|k| (k.dim.raw(), k.kind.raw(), k.rx, k.rz))
        .collect();
    for coord in universe {
        if skipped.contains(&(
            coord.dim.raw(),
            coord.kind.raw(),
            coord.region_x(),
            coord.region_z(),
        )) {
            present.insert(*coord);
        }
    }
}

/// Hash one raw chunk, flush it to CAS, and stage its history row.
///
/// Returns the time spent hashing and the time spent in `CAS put`
/// separately so digest and exists-check/write costs stay visible
/// in [`BackupTimings`](crate::timing::BackupTimings).
fn ingest_timed(
    chunk: RawChunk<'_>,
    cas: &mut FileCas,
    entries: &mut Vec<SnapshotEntry>,
    present: &mut HashSet<ChunkCoord>,
    new_blobs: &mut usize,
) -> Result<(Duration, Duration), EngineError> {
    let hash_started = Instant::now();
    let mut hasher = <Blake3Hasher as sekai_core::BlobHasher>::new();
    hasher.update(chunk.payload);
    let hash = hasher.finalize();
    let hash_elapsed = hash_started.elapsed();
    let cas_started = Instant::now();
    // Pinned to the `BlobStore` seam (not the inherent method) so the call
    // site only depends on the trait.
    if sekai_core::BlobStore::put(cas, &hash, chunk.payload)? {
        *new_blobs += 1;
    }
    let cas_elapsed = cas_started.elapsed();
    entries.push(SnapshotEntry::new(chunk.coord, Some(hash), None));
    present.insert(chunk.coord);
    Ok((hash_elapsed, cas_elapsed))
}
