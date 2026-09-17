//! Per-chunk facts on the snapshot timeline.

use super::{BlobHash, ChunkCoord, DiffHash, SnapshotId};

/// History row for one snapshot and chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkHistoryEntry {
    /// Which chunk this row describes.
    pub coord: ChunkCoord,
    /// Snapshot this row belongs to.
    pub snapshot: SnapshotId,
    /// Exact CAS key, or `None` for a tombstone.
    pub blob: Option<BlobHash>,
    /// Cached volatile hash, or `None` when not computed.
    pub diff: Option<DiffHash>,
}

impl ChunkHistoryEntry {
    /// Construct a history row.
    pub const fn new(
        coord: ChunkCoord,
        snapshot: SnapshotId,
        blob: Option<BlobHash>,
        diff: Option<DiffHash>,
    ) -> Self {
        Self {
            coord,
            snapshot,
            blob,
            diff,
        }
    }

    /// Whether this row is a tombstone (chunk absent at this snapshot).
    pub const fn is_tombstone(self) -> bool {
        self.blob.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Dimension, RegionKind};

    #[test]
    fn tombstone_detection() {
        let coord = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::REGION, 0, 0);
        let present = ChunkHistoryEntry::new(coord, SnapshotId(1), Some(BlobHash([1; 32])), None);
        assert!(!present.is_tombstone());
        let tomb = ChunkHistoryEntry::new(coord, SnapshotId(2), None, None);
        assert!(tomb.is_tombstone());
    }
}
