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
use std::time::{SystemTime, UNIX_EPOCH};

use sekai_core::{BlobHasher as _, ChunkCoord, MetaStore as _, RawChunk, RegionReader as _};
use sekai_mca::RegionFile;
use sekai_storage::SnapshotEntry;

use crate::discover::discover;
use crate::error::EngineError;
use crate::hash::Blake3Hasher;
use crate::store::Store;

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
    // Known universe = coordinates of the latest snapshot (empty on first run).
    let mut universe: HashSet<ChunkCoord> = HashSet::new();
    if let Some(latest) = store.meta().latest_snapshot()? {
        store.meta().visit_snapshot_chunks(latest.id, |entry| {
            universe.insert(entry.coord);
            true
        })?;
    }

    let mut entries: Vec<SnapshotEntry> = Vec::new();
    let mut present: HashSet<ChunkCoord> = HashSet::new();
    let mut new_blobs = 0usize;
    for region in discover(world)? {
        let file = RegionFile::open(&region.path, region.dim, region.kind)?;
        let mut failure: Option<EngineError> = None;
        file.visit_chunks(|chunk| {
            if failure.is_some() {
                return false;
            }
            if let Err(err) = ingest(chunk, store, &mut entries, &mut present, &mut new_blobs) {
                failure = Some(err);
                return false;
            }
            true
        })?;
        if let Some(err) = failure {
            return Err(err);
        }
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
    let snapshot = store.meta_mut().apply_snapshot(now_ms, &entries)?;
    Ok(BackupReport {
        snapshot,
        chunks: present.len(),
        new_blobs,
        tombstones,
    })
}

/// Hash one raw chunk, flush it to CAS, and stage its history row.
fn ingest(
    chunk: RawChunk<'_>,
    store: &mut Store,
    entries: &mut Vec<SnapshotEntry>,
    present: &mut HashSet<ChunkCoord>,
    new_blobs: &mut usize,
) -> Result<(), EngineError> {
    let mut hasher = <Blake3Hasher as sekai_core::BlobHasher>::new();
    hasher.update(chunk.payload);
    let hash = hasher.finalize();
    // Pinned to the `BlobStore` seam (not the inherent method) so the call
    // site only depends on the trait.
    if sekai_core::BlobStore::put(store.cas_mut(), &hash, chunk.payload)? {
        *new_blobs += 1;
    }
    entries.push(SnapshotEntry::new(chunk.coord, Some(hash), None));
    present.insert(chunk.coord);
    Ok(())
}
