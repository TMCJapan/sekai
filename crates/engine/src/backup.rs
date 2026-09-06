//! Full-world backup: scan, deduplicate into CAS, record one snapshot.
//!
//! Rationale: backup never parses NBT - `blob_hash` runs over raw sector
//! bytes, so the hot path is read + hash + CAS write with zero decoding.
//! Change detection (`diff`) is deliberately not computed here (MVP
//! records `None`); parsing every chunk would triple scan cost for data no
//! consumer reads yet.
//!
//! Tombstones keep history total without a global chunk census: the known
//! universe is exactly the coordinate set of the latest snapshot, so every
//! snapshot re-records every known coordinate (present or tombstone) and
//! the induction holds from the first backup on.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sekai_core::{BlobHasher as _, ChunkCoord, MetaStore as _, RawChunk, RegionReader as _};
use sekai_mca::RegionFile;
use sekai_storage::SnapshotEntry;

use crate::discover::discover;
use crate::error::EngineError;
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

    // Known universe = coordinates of the latest snapshot (empty on first run).
    let universe_started = Instant::now();
    let mut universe: HashSet<ChunkCoord> = HashSet::new();
    if let Some(latest) = store.meta().latest_snapshot()? {
        store.meta().visit_snapshot_chunks(latest.id, |entry| {
            universe.insert(entry.coord);
            true
        })?;
    }
    let universe_load = universe_started.elapsed();

    let discover_started = Instant::now();
    let regions = discover(world)?;
    let discover = discover_started.elapsed();

    let mut entries: Vec<SnapshotEntry> = Vec::new();
    let mut present: HashSet<ChunkCoord> = HashSet::new();
    let mut new_blobs = 0usize;
    let mut region_open = Duration::ZERO;
    let mut ingest_sum = Duration::ZERO;
    let mut hash_sum = Duration::ZERO;
    let mut cas_sum = Duration::ZERO;
    let mut region_timings = Vec::with_capacity(regions.len());
    for region in regions {
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
            match ingest_timed(chunk, store, &mut entries, &mut present, &mut new_blobs) {
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
        region_open += open_elapsed;
        ingest_sum += ingest_elapsed;
        hash_sum += hash_elapsed;
        cas_sum += cas_elapsed;
        region_timings.push(RegionTiming {
            path: region.path,
            bytes,
            chunks,
            open: open_elapsed,
            ingest: ingest_elapsed,
            hash: hash_elapsed,
            cas: cas_elapsed,
        });
    }

    let mut tombstones = 0usize;
    for coord in &universe {
        if !present.contains(coord) {
            entries.push(SnapshotEntry::new(*coord, None, None));
            tombstones += 1;
        }
    }

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))?;
    let db_started = Instant::now();
    let snapshot = store.meta_mut().apply_snapshot(now_ms, &entries)?;
    let db_apply = db_started.elapsed();

    let report = BackupReport {
        snapshot,
        chunks: present.len(),
        new_blobs,
        tombstones,
    };
    let timings = BackupTimings {
        total: total.elapsed(),
        discover,
        universe_load,
        region_open,
        ingest: ingest_sum,
        hash: hash_sum,
        cas_put: cas_sum,
        cas_checked: present.len(),
        db_apply,
        regions: region_timings,
    };
    Ok((report, timings))
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
