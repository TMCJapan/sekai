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
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sekai_core::{BlobHasher as _, ChunkCoord, MetaStore as _, RawChunk, RegionReader as _};
use sekai_mca::RegionFile;
use sekai_storage::{RegionFingerprint, RegionKey, RegionStateEntry, SnapshotEntry};

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
        cas_put: walk.cas,
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
        // Unchanged files skip read/hash/CAS; their rows carry over below.
        if states
            .get(&key_tuple)
            .is_some_and(|st| observed.matches_state(st))
        {
            walk.carries.push(key);
            continue;
        }
        ingest_file(region, observed, store, &mut walk)?;
    }
    Ok(walk)
}

/// Read, hash, and store one changed region file, staging its rows.
fn ingest_file(
    region: &RegionRef,
    observed: RegionFingerprint,
    store: &mut Store,
    walk: &mut RegionWalk,
) -> Result<(), EngineError> {
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
        match ingest_timed(
            chunk,
            store,
            &mut walk.entries,
            &mut walk.present,
            &mut walk.new_blobs,
        ) {
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
    walk.region_open += open_elapsed;
    walk.ingest += ingest_elapsed;
    walk.hash += hash_elapsed;
    walk.cas += cas_elapsed;
    walk.fingerprints.push(observed);
    walk.timings.push(RegionTiming {
        path: region.path.clone(),
        bytes,
        chunks,
        open: open_elapsed,
        ingest: ingest_elapsed,
        hash: hash_elapsed,
        cas: cas_elapsed,
    });
    Ok(())
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
    store: &mut Store,
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
    if sekai_core::BlobStore::put(store.cas_mut(), &hash, chunk.payload)? {
        *new_blobs += 1;
    }
    let cas_elapsed = cas_started.elapsed();
    entries.push(SnapshotEntry::new(chunk.coord, Some(hash), None));
    present.insert(chunk.coord);
    Ok((hash_elapsed, cas_elapsed))
}
