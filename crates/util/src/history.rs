//! Per-chunk facts on the commit history graph.

use super::{BlobHash, ChunkCoord, CommitId, DiffHash};

/// History row for one commit and chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkHistoryEntry {
    /// Which chunk this row describes.
    pub coord: ChunkCoord,
    /// Commit this row belongs to.
    pub commit: CommitId,
    /// Exact CAS key, or `None` for a tombstone.
    pub blob: Option<BlobHash>,
    /// Cached volatile hash, or `None` when not computed.
    pub diff: Option<DiffHash>,
}

impl ChunkHistoryEntry {
    /// Construct a history row.
    pub const fn new(
        coord: ChunkCoord,
        commit: CommitId,
        blob: Option<BlobHash>,
        diff: Option<DiffHash>,
    ) -> Self {
        Self {
            coord,
            commit,
            blob,
            diff,
        }
    }

    /// Whether this row is a tombstone (chunk absent at this commit).
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
        let coord = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
        let present =
            ChunkHistoryEntry::new(coord, CommitId([1; 32]), Some(BlobHash([1; 32])), None);
        assert!(!present.is_tombstone());
        let tomb = ChunkHistoryEntry::new(coord, CommitId([2; 32]), None, None);
        assert!(tomb.is_tombstone());
    }
}
