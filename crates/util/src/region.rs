//! Region identities and observed file fingerprints.

use super::{BlobHash, ChunkCoord, CommitId, DiffHash, Dimension, RegionKind};

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

/// Fingerprint observed on disk.
///
/// Commit decisions compare per-chunk history instead, so this is an
/// observation/diagnostic value rather than a persisted cache key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionFingerprint {
    /// Which file this fingerprint describes.
    pub key: RegionKey,
    /// Last modification time as Unix millis (`None` when unavailable).
    pub mtime_ms: Option<u64>,
    /// File size in bytes.
    pub size: u64,
    /// Blake3 of the whole file.
    pub content_hash: [u8; 32],
}

/// Chunk state staged for a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommitEntry {
    /// Which chunk this fact describes.
    pub coord: ChunkCoord,
    /// Exact CAS key, or `None` for a tombstone.
    pub blob: Option<BlobHash>,
    /// Cached volatile hash, or `None` when not computed.
    pub diff: Option<DiffHash>,
}

impl CommitEntry {
    /// Construct a chunk fact.
    pub const fn new(coord: ChunkCoord, blob: Option<BlobHash>, diff: Option<DiffHash>) -> Self {
        Self { coord, blob, diff }
    }
}

/// Result of recording a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplyOutcome {
    /// Newly recorded commit ID.
    pub id: CommitId,
}

/// Result of retiring one commit into its successor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FoldOutcome {
    /// Rows re-stamped onto the successor commit.
    pub folded: usize,
    /// Rows superseded before the successor and removed.
    pub dropped: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_key_of_chunk() {
        let coord = ChunkCoord::new(Dimension::NETHER, RegionKind::Poi, -33, 70);
        assert_eq!(
            RegionKey::of(coord),
            RegionKey::new(Dimension::NETHER, RegionKind::Poi, -2, 2)
        );
    }
}
