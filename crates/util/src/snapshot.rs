//! Snapshot identifiers and metadata.

use alloc::vec::Vec;

use crate::tag::TagName;

/// Monotonically increasing snapshot identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SnapshotId(pub u64);

impl SnapshotId {
    /// Raw ID (for display and integer columns).
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Recorded backup metadata.
///
/// A snapshot is a node of the history graph: `parents` names the
/// snapshots it continues. The list is never null - it is empty for the
/// root of a history, holds one entry for an ordinary snapshot, and
/// several for a merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Snapshot identifier.
    pub id: SnapshotId,
    /// Creation time as Unix millis (informational only).
    pub created_at_ms: u64,
    /// Snapshots this one descends from.
    pub parents: Vec<SnapshotId>,
}

impl Snapshot {
    /// Construct snapshot metadata.
    pub const fn new(id: SnapshotId, created_at_ms: u64, parents: Vec<SnapshotId>) -> Self {
        Self {
            id,
            created_at_ms,
            parents,
        }
    }

    /// Whether the snapshot starts a history (no parents).
    pub const fn is_root(&self) -> bool {
        self.parents.is_empty()
    }
}

/// Named alias for one snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotTag {
    /// Validated tag name.
    pub name: TagName,
    /// Snapshot the tag points at.
    pub snapshot: SnapshotId,
    /// Tag creation time as Unix millis (informational only).
    pub created_at_ms: u64,
}

impl SnapshotTag {
    /// Construct a tag record.
    pub const fn new(name: TagName, snapshot: SnapshotId, created_at_ms: u64) -> Self {
        Self {
            name,
            snapshot,
            created_at_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_ids_and_metadata() {
        let id = SnapshotId(7);
        assert_eq!(id.raw(), 7);
        let snapshot = Snapshot::new(id, 1_700_000_000_000, alloc::vec![SnapshotId(6)]);
        assert_eq!(snapshot.id, id);
        assert_eq!(snapshot.created_at_ms, 1_700_000_000_000);
        assert_eq!(snapshot.parents, [SnapshotId(6)]);
        assert!(!snapshot.is_root());
        assert!(Snapshot::new(id, 0, alloc::vec![]).is_root());
        assert!(SnapshotId(1) < SnapshotId(2));
    }
}
