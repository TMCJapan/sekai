//! Snapshot identifiers and metadata.

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    /// Snapshot identifier.
    pub id: SnapshotId,
    /// Creation time as Unix millis (informational only).
    pub created_at_ms: u64,
}

impl Snapshot {
    /// Construct snapshot metadata.
    pub const fn new(id: SnapshotId, created_at_ms: u64) -> Self {
        Self { id, created_at_ms }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_ids_and_metadata() {
        let id = SnapshotId(7);
        assert_eq!(id.raw(), 7);
        let snapshot = Snapshot::new(id, 1_700_000_000_000);
        assert_eq!(snapshot.id, id);
        assert_eq!(snapshot.created_at_ms, 1_700_000_000_000);
        assert!(SnapshotId(1) < SnapshotId(2));
    }
}
