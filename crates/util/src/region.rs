//! Region identities and incremental-backup fingerprints.

use super::{BlobHash, ChunkCoord, DiffHash, Dimension, RegionKind, SnapshotId};

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
    pub const fn new(dim: Dimension, kind: RegionKind, rx: i32, rz: i32) -> Self {
        Self { dim, kind, rx, rz }
    }

    /// Region identity owning a chunk coordinate.
    pub const fn of(coord: ChunkCoord) -> Self {
        Self::new(coord.dim, coord.kind, coord.region_x(), coord.region_z())
    }
}

/// Fingerprint observed on disk during backup planning.
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

/// Persisted fingerprint of the last ingested file state.
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
    /// Snapshot that last confirmed this fingerprint.
    pub snapshot_id: SnapshotId,
}

impl RegionFingerprint {
    /// Whether the file can skip ingestion against stored state.
    ///
    /// All three signals must agree, and a missing `mtime` on either side
    /// forces ingest: silently trusting a clock the platform cannot provide
    /// would risk stale snapshots, so the fail-safe direction is to redo
    /// the work.
    pub fn matches_state(&self, state: &RegionStateEntry) -> bool {
        self.key == state.key
            && self.mtime_ms == state.mtime_ms
            && self.mtime_ms.is_some()
            && self.size == state.size
            && self.header_hash == state.header_hash
    }
}

/// Chunk state staged for a snapshot commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotEntry {
    /// Which chunk this fact describes.
    pub coord: ChunkCoord,
    /// Exact CAS key, or `None` for a tombstone.
    pub blob: Option<BlobHash>,
    /// Cached volatile hash, or `None` when not computed.
    pub diff: Option<DiffHash>,
}

impl SnapshotEntry {
    /// Construct a chunk fact.
    pub const fn new(coord: ChunkCoord, blob: Option<BlobHash>, diff: Option<DiffHash>) -> Self {
        Self { coord, blob, diff }
    }
}

/// Result of a snapshot commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplyOutcome {
    /// Newly recorded snapshot ID.
    pub id: SnapshotId,
    /// History rows carried over from the previous snapshot.
    pub carried_chunks: usize,
}

/// Result of retiring one snapshot into its successor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FoldOutcome {
    /// Rows re-stamped onto the successor snapshot.
    pub folded: usize,
    /// Rows superseded before the successor and removed.
    pub dropped: usize,
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

    #[test]
    fn region_key_of_chunk() {
        let coord = ChunkCoord::new(Dimension::NETHER, RegionKind::POI, -33, 70);
        assert_eq!(
            RegionKey::of(coord),
            RegionKey::new(Dimension::NETHER, RegionKind::POI, -2, 2)
        );
    }
}
