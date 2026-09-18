//! MVCC metadata storage boundary.

use alloc::vec::Vec;
use core::future::Future;
use sekai_util::{
    ApplyOutcome, BlobHash, ChunkCoord, ChunkHistoryEntry, DiffHash, FoldOutcome,
    RegionFingerprint, RegionKey, RegionStateEntry, Snapshot, SnapshotEntry, SnapshotId,
    SnapshotTag, TagName,
};

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
        coord: &ChunkCoord,
        blob: Option<&BlobHash>,
        diff: Option<&DiffHash>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Effective row for (`snapshot`, `coord`): the nearest row at or
    /// before `snapshot`, or `None` when never recorded.
    fn lookup_chunk(
        &self,
        snapshot: SnapshotId,
        coord: &ChunkCoord,
    ) -> impl Future<Output = Result<Option<ChunkHistoryEntry>, Self::Error>> + Send;

    /// Snapshot metadata for `id`, or `None` when it does not exist.
    fn lookup_snapshot(
        &self,
        id: SnapshotId,
    ) -> impl Future<Output = Result<Option<Snapshot>, Self::Error>> + Send;

    /// Highest-ID snapshot, or `None` when no backup has run yet.
    fn latest_snapshot(&self)
    -> impl Future<Output = Result<Option<Snapshot>, Self::Error>> + Send;

    /// Visit effective rows of one snapshot: exactly one row per
    /// coordinate known at `snapshot` (the nearest row at or before it),
    /// including tombstones. Return `false` to stop early.
    fn visit_snapshot_chunks<F>(
        &self,
        snapshot: SnapshotId,
        visit: F,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        F: FnMut(&ChunkHistoryEntry) -> bool + Send;

    /// Visit raw rows introduced at `snapshot` (fresh ingests plus
    /// tombstones, no fallback). Return `false` to stop early.
    fn visit_fresh_rows<F>(
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
    /// unchanged regions: backends advance their snapshot state and count
    /// their effective chunks, but write no per-chunk rows for them (delta
    /// storage). Fingerprints are refreshed atomically.
    ///
    /// Referenced blobs must already be flushed to CAS.
    fn apply_snapshot_incremental(
        &mut self,
        created_at_ms: u64,
        entries: &[SnapshotEntry],
        carry_from: Option<(SnapshotId, &[RegionKey])>,
        fingerprints: &[RegionFingerprint],
        removed: &[RegionKey],
    ) -> impl Future<Output = Result<ApplyOutcome, Self::Error>> + Send;

    /// Point `name` at `snapshot`. Duplicate names are a backend error;
    /// callers check-then-insert when they need friendlier failures.
    fn tag_snapshot(
        &mut self,
        name: &TagName,
        snapshot: SnapshotId,
        created_at_ms: u64,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Remove `name`. Returns whether a tag was removed.
    fn untag(&mut self, name: &TagName) -> impl Future<Output = Result<bool, Self::Error>> + Send;

    /// Tag record for `name`, or `None` when it does not exist.
    fn lookup_tag(
        &self,
        name: &TagName,
    ) -> impl Future<Output = Result<Option<SnapshotTag>, Self::Error>> + Send;

    /// Visit tags in name order. Return `false` to stop early.
    fn visit_tags<F>(&self, visit: F) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        F: FnMut(&SnapshotTag) -> bool + Send;

    /// Retire `from` into `into` (`into` must be retained and newer):
    /// rows superseded at or before `into` are removed, surviving rows
    /// are re-stamped onto `into`, derived states follow, and the
    /// snapshot row (plus its tags, by cascade) is deleted — all
    /// atomically. Effective states of retained snapshots never change.
    fn retire_snapshot(
        &mut self,
        from: SnapshotId,
        into: SnapshotId,
    ) -> impl Future<Output = Result<FoldOutcome, Self::Error>> + Send;
}
