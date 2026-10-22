//! Snapshot metadata and per-chunk history storage.
//!
//! Snapshots form a history graph: every snapshot names the snapshots it
//! continues (`parents`). A store therefore answers "what did this
//! snapshot inherit?" by walking parent links, not by comparing
//! identifiers.

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

    /// Append a snapshot continuing `parents`; an empty list starts a
    /// history. IDs are monotone increasing.
    ///
    /// Every parent must exist, a snapshot may not name itself, and a
    /// parent listed twice is recorded once.
    fn create_snapshot(
        &mut self,
        created_at_ms: u64,
        parents: &[SnapshotId],
    ) -> impl Future<Output = Result<SnapshotId, Self::Error>> + Send;

    /// Direct parents of `snapshot`; empty for the root of a history.
    ///
    /// An unknown snapshot yields an empty list: the port reports what
    /// is stored, not what exists. The use cases above it check for
    /// existence first.
    fn snapshot_parents(
        &self,
        snapshot: SnapshotId,
    ) -> impl Future<Output = Result<Vec<SnapshotId>, Self::Error>> + Send;

    /// Record one chunk's state at `snapshot`. `blob = None` is a tombstone.
    fn record_chunk(
        &mut self,
        snapshot: SnapshotId,
        coord: &ChunkCoord,
        blob: Option<&BlobHash>,
        diff: Option<&DiffHash>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Effective row for (`snapshot`, `coord`): the row introduced at
    /// `snapshot` or at one of its ancestors. Of the ancestor rows, the
    /// one with the highest snapshot ID wins, or `None` when no
    /// ancestor ever recorded the coordinate.
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
    /// coordinate known at `snapshot` (the row introduced at it or at one
    /// of its ancestors, the highest snapshot ID winning), including
    /// tombstones. Return `false` to stop early.
    ///
    /// The effective set is materialized before the visitor runs, so the
    /// early return saves iteration, not the read. Resolving it costs
    /// the ancestry of the snapshot: a caller that walks many snapshots
    /// pays for the history depth per snapshot.
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
    /// The new snapshot continues `parents`; an empty list starts a
    /// history. As in [`Self::create_snapshot`], every parent must
    /// exist, a snapshot may not name itself, and a parent listed twice
    /// is recorded once. `entries` contains fresh rows and tombstones.
    /// `carry_from` identifies unchanged regions: backends advance their
    /// snapshot state and count their effective chunks, but write no
    /// per-chunk rows for them (delta storage). Fingerprints are
    /// refreshed atomically, and `removed` names regions whose state
    /// rows drop: files gone from disk or ingested only partly.
    ///
    /// Referenced blobs must already be flushed to CAS.
    fn apply_snapshot_incremental(
        &mut self,
        created_at_ms: u64,
        parents: &[SnapshotId],
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
    /// rows superseded along the history `into` continues are removed,
    /// surviving rows are re-stamped onto `into`, derived states follow,
    /// and the snapshot row (plus its tags, by cascade) is deleted - all
    /// atomically. The children of `from` continue the history of its
    /// own parents, so the graph stays connected. Effective states of
    /// retained snapshots never change.
    ///
    /// `into` must be a descendant of `from`: folding into an unrelated
    /// history would graft rows the successor never had.
    fn retire_snapshot(
        &mut self,
        from: SnapshotId,
        into: SnapshotId,
    ) -> impl Future<Output = Result<FoldOutcome, Self::Error>> + Send;
}
