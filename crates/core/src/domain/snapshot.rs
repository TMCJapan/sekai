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
