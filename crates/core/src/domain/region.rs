//! Region identity and derived per-region fingerprints.
//!
//! Rationale: incremental snapshots skip byte-identical region files. Each
//! file is identified by its namespace and region coordinates
//! ([`RegionKey`]); the `(mtime, size, header hash)` observed when it was
//! last ingested is staged as a [`RegionFingerprint`] and stored as a
//! [`RegionStateEntry`]. The table is purely derived: wiping it degrades the
//! next backup to a full ingest, never to wrong data. [`SnapshotEntry`] is
//! the staging fact for one chunk row, and [`ApplyOutcome`] reports what a
//! snapshot commit recorded.

use super::coords::{ChunkCoord, Dimension, RegionKind};
use super::hash::{BlobHash, DiffHash};
use super::snapshot::SnapshotId;

/// Identity of one region file within its namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegionKey {
    /// Dimension namespace.
    pub dim: Dimension,
    /// Region family.
    pub kind: RegionKind,
    /// Region X from the file name.
    pub rx: i32,
    /// Region Z from the file name.
    pub rz: i32,
}

impl RegionKey {
    /// Construct a region identity.
    #[must_use]
    pub const fn new(dim: Dimension, kind: RegionKind, rx: i32, rz: i32) -> Self {
        Self { dim, kind, rx, rz }
    }
}

/// Freshly observed file fingerprint, staged for the derived state table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionFingerprint {
    /// Which file this fingerprint describes.
    pub key: RegionKey,
    /// Last modification time as Unix millis (`None` when unavailable).
    pub mtime_ms: Option<u64>,
    /// File size in bytes.
    pub size: u64,
    /// Blake3 of the file's location-table sector (first 4 KiB).
    pub header_hash: [u8; 32],
}

/// Stored fingerprint of the snapshot that last ingested the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionStateEntry {
    /// Which file this state describes.
    pub key: RegionKey,
    /// Last modification time as Unix millis (`None` when unavailable).
    pub mtime_ms: Option<u64>,
    /// File size in bytes.
    pub size: u64,
    /// Blake3 of the file's location-table sector (first 4 KiB).
    pub header_hash: [u8; 32],
    /// Snapshot that last confirmed this fingerprint (ingested, or carried
    /// over by a fingerprint match).
    pub snapshot_id: SnapshotId,
}

impl RegionFingerprint {
    /// Whether the file can skip ingestion against stored state.
    ///
    /// All three signals must agree, and a missing `mtime` on either side
    /// forces ingest: silently trusting a clock the platform cannot provide
    /// would risk stale snapshots, so the fail-safe direction is to redo
    /// the work. A same-size in-place payload rewrite that keeps the
    /// location table unchanged inside one `mtime` granularity tick still
    /// slips through; filesystems with coarse timestamps (e.g. FAT with 2 s
    /// granularity) widen that window, so callers must quiesce the server
    /// before snapshotting.
    #[must_use]
    pub fn matches_state(&self, state: &RegionStateEntry) -> bool {
        if self.key != state.key {
            return false;
        }
        if self.mtime_ms != state.mtime_ms || self.mtime_ms.is_none() {
            return false;
        }
        self.size == state.size && self.header_hash == state.header_hash
    }
}

/// One chunk fact staged for a snapshot commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotEntry {
    /// Which chunk this fact describes.
    pub coord: ChunkCoord,
    /// Exact CAS key, or `None` for a tombstone.
    pub blob: Option<BlobHash>,
    /// Cached volatile hash, or `None` when not computed (backup always
    /// records `None`; the column only reserves the cache).
    pub diff: Option<DiffHash>,
}

impl SnapshotEntry {
    /// Construct a chunk fact.
    #[must_use]
    pub const fn new(coord: ChunkCoord, blob: Option<BlobHash>, diff: Option<DiffHash>) -> Self {
        Self { coord, blob, diff }
    }
}

/// Outcome of a snapshot commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplyOutcome {
    /// Newly recorded snapshot ID.
    pub id: SnapshotId,
    /// History rows carried over from the previous snapshot.
    pub carried_chunks: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn key() -> RegionKey {
        RegionKey::new(Dimension::OVERWORLD, RegionKind::REGION, 0, 0)
    }

    const fn fingerprint() -> RegionFingerprint {
        RegionFingerprint {
            key: key(),
            mtime_ms: Some(1_700_000_000_000),
            size: 8192,
            header_hash: [7; 32],
        }
    }

    const fn stored() -> RegionStateEntry {
        RegionStateEntry {
            key: key(),
            mtime_ms: Some(1_700_000_000_000),
            size: 8192,
            header_hash: [7; 32],
            snapshot_id: SnapshotId(1),
        }
    }

    #[test]
    fn fingerprint_match_requires_all_signals() {
        let base = stored();
        assert!(fingerprint().matches_state(&base));

        let mut changed = fingerprint();
        changed.mtime_ms = Some(1_700_000_000_001);
        assert!(!changed.matches_state(&base));

        let mut changed = fingerprint();
        changed.size += 1;
        assert!(!changed.matches_state(&base));

        let mut changed = fingerprint();
        changed.header_hash[0] ^= 0xFF;
        assert!(!changed.matches_state(&base));

        // Unknown clock on either side forces ingest (fail-safe direction).
        let mut changed = fingerprint();
        changed.mtime_ms = None;
        assert!(!changed.matches_state(&base));
        let mut stored = base;
        stored.mtime_ms = None;
        assert!(!fingerprint().matches_state(&stored));
    }
}
