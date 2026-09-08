//! MVCC metadata port over snapshots and per-chunk history.
//!
//! Rationale: history rows are plain facts; interval semantics emerge from
//! snapshot order. The incremental-commit seam
//! ([`MetaStore::apply_snapshot_incremental`]) keeps the carry optimization
//! (one indexed `INSERT ... SELECT` per unchanged region instead of a
//! per-chunk loop) behind the port, so use cases never issue SQL while the
//! SQLite layout stays an adapter detail.

use crate::domain::history::ChunkHistoryEntry;
use crate::domain::region::{
    ApplyOutcome, RegionFingerprint, RegionKey, RegionStateEntry, SnapshotEntry,
};
use crate::domain::snapshot::{Snapshot, SnapshotId};

/// MVCC metadata over snapshots and per-chunk history (by `storage`).
pub trait MetaStore {
    /// Backend failure (SQLite errors, constraint violations, ...).
    type Error;

    /// Append a snapshot; IDs are monotone increasing.
    fn create_snapshot(&mut self, created_at_ms: u64) -> Result<SnapshotId, Self::Error>;

    /// Record one chunk's state at `snapshot`. `blob = None` is a tombstone.
    fn record_chunk(
        &mut self,
        snapshot: SnapshotId,
        coord: &crate::domain::coords::ChunkCoord,
        blob: Option<&crate::domain::hash::BlobHash>,
        diff: Option<&crate::domain::hash::DiffHash>,
    ) -> Result<(), Self::Error>;

    /// Exact row for (`snapshot`, `coord`), or `None` when never recorded.
    fn lookup_chunk(
        &self,
        snapshot: SnapshotId,
        coord: &crate::domain::coords::ChunkCoord,
    ) -> Result<Option<ChunkHistoryEntry>, Self::Error>;

    /// Snapshot metadata for `id`, or `None` when it does not exist.
    ///
    /// Point query behind the seam so callers never scan the full timeline
    /// to resolve one snapshot.
    fn lookup_snapshot(&self, id: SnapshotId) -> Result<Option<Snapshot>, Self::Error>;

    /// Highest-ID snapshot, or `None` when no backup has run yet.
    fn latest_snapshot(&self) -> Result<Option<Snapshot>, Self::Error>;

    /// Visit every row of one snapshot. Return `false` to stop early.
    ///
    /// Visitor style keeps full-snapshot rollback streaming without
    /// materializing all rows (1024+ per region file) in memory.
    fn visit_snapshot_chunks<F>(&self, snapshot: SnapshotId, visit: F) -> Result<(), Self::Error>
    where
        F: FnMut(&ChunkHistoryEntry) -> bool;

    /// Visit snapshots in ID order. Return `false` to stop early.
    fn visit_snapshots<F>(&self, visit: F) -> Result<(), Self::Error>
    where
        F: FnMut(&Snapshot) -> bool;

    /// Load every stored region fingerprint (for skip decisions in backup).
    fn load_region_states(&self) -> Result<alloc::vec::Vec<RegionStateEntry>, Self::Error>;

    /// Record a snapshot, carrying unchanged regions from `carry_from`.
    ///
    /// `entries` holds freshly ingested chunks and tombstones; `carry_from`
    /// names the previous snapshot plus the regions whose rows copy over.
    /// `fingerprints` refreshes the derived state for ingested files,
    /// carried keys keep their fingerprints but advance to the new
    /// snapshot, and `removed` drops state for files gone from disk, all
    /// atomically so state and history stay consistent. Carried regions
    /// must be disjoint from `entries`; overlap aborts loudly instead of
    /// merging silently. Blobs must already be flushed to CAS before
    /// calling.
    fn apply_snapshot_incremental(
        &mut self,
        created_at_ms: u64,
        entries: &[SnapshotEntry],
        carry_from: Option<(SnapshotId, &[RegionKey])>,
        fingerprints: &[RegionFingerprint],
        removed: &[RegionKey],
    ) -> Result<ApplyOutcome, Self::Error>;
}
