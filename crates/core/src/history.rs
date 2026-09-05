//! Per-chunk history rows over the snapshot timeline.
//!
//! Rationale: each row covers the interval `[snapshot, next_snapshot)` for
//! one coordinate. A `None` blob is an explicit tombstone (chunk absent in
//! that snapshot) so rollback can truncate the sector instead of leaving
//! stale data behind. `diff` is an optional cached view that must always
//! be recomputable from blobs and is never trusted for restores.

use crate::coords::ChunkCoord;
use crate::hash::{BlobHash, DiffHash};
use crate::snapshot::SnapshotId;

/// State of one chunk at one snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkHistoryEntry {
    /// Which chunk this row describes.
    pub coord: ChunkCoord,
    /// Timeline position this row belongs to.
    pub snapshot: SnapshotId,
    /// Exact CAS key, or `None` for a tombstone (chunk absent).
    pub blob: Option<BlobHash>,
    /// Cached volatile change-detection hash (recomputable, never a key).
    pub diff: Option<DiffHash>,
}

impl ChunkHistoryEntry {
    /// Construct a history row.
    #[inline]
    #[must_use]
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

    /// Whether this row records absence rather than data.
    #[inline]
    #[must_use]
    pub const fn is_tombstone(self) -> bool {
        self.blob.is_none()
    }
}
