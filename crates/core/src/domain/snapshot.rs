//! Snapshot identity and metadata.
//!
//! Rationale: snapshots form an append-only MVCC timeline (not a commit
//! DAG), so the ID is a monotone `u64` matching the SQLite `rowid`.
//! `created_at_ms` is wall-clock metadata only and never participates in
//! chunk identity or rollback ordering.

/// Monotone snapshot identifier (1-based SQLite rowid).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SnapshotId(pub u64);

impl SnapshotId {
    /// Raw numeric identifier.
    #[inline]
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// One backup point in the MVCC timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    /// Timeline position.
    pub id: SnapshotId,
    /// Wall-clock creation time (unix millis, informational only).
    pub created_at_ms: u64,
}

impl Snapshot {
    /// Construct snapshot metadata.
    #[inline]
    #[must_use]
    pub const fn new(id: SnapshotId, created_at_ms: u64) -> Self {
        Self { id, created_at_ms }
    }
}
