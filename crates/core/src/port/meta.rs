//! MVCC metadata storage boundary.

use crate::domain::history::ChunkHistoryEntry;
use crate::domain::region::{
    ApplyOutcome, RegionFingerprint, RegionKey, RegionStateEntry, SnapshotEntry,
};
use crate::domain::snapshot::{Snapshot, SnapshotId};
use alloc::vec::Vec;
use core::future::Future;

/// Snapshot metadata and per-chunk history storage.
pub trait MetaStore {
    /// Backend failure (query errors, constraint violations, ...).
    type Error;

    /// Append a snapshot; IDs are monotone increasing.
    fn create_snapshot(
        &mut self,
        created_at_ms: u64,
    ) -> impl Future<Output = Result<SnapshotId, Self::Error>> + Send;

    /// Record one chunk's state at `snapshot`. `blob = None` is a tombstone.
    fn record_chunk(
        &mut self,
        snapshot: SnapshotId,
        coord: &crate::domain::coords::ChunkCoord,
        blob: Option<&crate::domain::hash::BlobHash>,
        diff: Option<&crate::domain::hash::DiffHash>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Exact row for (`snapshot`, `coord`), or `None` when never recorded.
    fn lookup_chunk(
        &self,
        snapshot: SnapshotId,
        coord: &crate::domain::coords::ChunkCoord,
    ) -> impl Future<Output = Result<Option<ChunkHistoryEntry>, Self::Error>> + Send;

    /// Snapshot metadata for `id`, or `None` when it does not exist.
    fn lookup_snapshot(
        &self,
        id: SnapshotId,
    ) -> impl Future<Output = Result<Option<Snapshot>, Self::Error>> + Send;

    /// Highest-ID snapshot, or `None` when no backup has run yet.
    fn latest_snapshot(&self)
    -> impl Future<Output = Result<Option<Snapshot>, Self::Error>> + Send;

    /// Visit every row of one snapshot. Return `false` to stop early.
    ///
    /// Visitor style keeps full-snapshot rollback streaming without
    /// materializing all rows in memory.
    fn visit_snapshot_chunks<F>(
        &self,
        snapshot: SnapshotId,
        visit: F,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        F: FnMut(&ChunkHistoryEntry) -> bool + Send;

    /// Visit snapshots in ID order. Return `false` to stop early.
    fn visit_snapshots<F>(&self, visit: F) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        F: FnMut(&Snapshot) -> bool + Send;

    /// Load every stored region fingerprint (for skip decisions in backup).
    fn load_region_states(
        &self,
    ) -> impl Future<Output = Result<Vec<RegionStateEntry>, Self::Error>> + Send;

    /// Commit a snapshot and optionally carry unchanged regions.
    ///
    /// `entries` contains fresh rows and tombstones. `carry_from` identifies
    /// regions copied from the previous snapshot. Fingerprints are refreshed
    /// atomically; carried regions advance their snapshot state.
    ///
    /// Carried regions must not overlap `entries`, and referenced blobs must
    /// already be flushed to CAS.
    fn apply_snapshot_incremental(
        &mut self,
        created_at_ms: u64,
        entries: &[SnapshotEntry],
        carry_from: Option<(SnapshotId, &[RegionKey])>,
        fingerprints: &[RegionFingerprint],
        removed: &[RegionKey],
    ) -> impl Future<Output = Result<ApplyOutcome, Self::Error>> + Send;
}
