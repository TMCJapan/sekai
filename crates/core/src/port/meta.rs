//! Commit metadata and per-chunk history storage.
//!
//! Commits form a history graph: every commit names the commits it
//! continues (`parents`). A store therefore answers "what did this
//! commit inherit?" by walking parent links, not by comparing
//! identifiers.

use alloc::vec::Vec;
use core::future::Future;
use sekai_util::{
    ApplyOutcome, BlobHash, ChunkCoord, ChunkHistoryEntry, Commit, CommitEntry, CommitId,
    CommitTag, DiffHash, FoldOutcome, RegionFingerprint, RegionKey, RegionStateEntry, TagName,
};

/// Commit metadata and per-chunk history storage.
pub trait MetaStore {
    /// Backend failure (query errors, constraint violations, ...).
    type Error;

    /// Append a commit continuing `parents`; an empty list starts a
    /// history. IDs are monotone increasing.
    ///
    /// Every parent must exist, a commit may not name itself, and a
    /// parent listed twice is recorded once.
    fn create_commit(
        &mut self,
        created_at_ms: u64,
        parents: &[CommitId],
    ) -> impl Future<Output = Result<CommitId, Self::Error>> + Send;

    /// Direct parents of `commit`; empty for the root of a history.
    ///
    /// An unknown commit yields an empty list: the port reports what
    /// is stored, not what exists. The use cases above it check for
    /// existence first.
    fn commit_parents(
        &self,
        commit: CommitId,
    ) -> impl Future<Output = Result<Vec<CommitId>, Self::Error>> + Send;

    /// Record one chunk's state at the commit `commit`. `blob = None` is a
    /// tombstone.
    fn record_chunk(
        &mut self,
        commit: CommitId,
        coord: &ChunkCoord,
        blob: Option<&BlobHash>,
        diff: Option<&DiffHash>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Effective row for (`commit`, `coord`): the row introduced at
    /// `commit` or at one of its ancestors. Of the ancestor rows, the
    /// one with the highest commit ID wins, or `None` when no
    /// ancestor ever recorded the coordinate.
    fn lookup_chunk(
        &self,
        commit: CommitId,
        coord: &ChunkCoord,
    ) -> impl Future<Output = Result<Option<ChunkHistoryEntry>, Self::Error>> + Send;

    /// Commit metadata for `id`, or `None` when it does not exist.
    fn lookup_commit(
        &self,
        id: CommitId,
    ) -> impl Future<Output = Result<Option<Commit>, Self::Error>> + Send;

    /// Highest-ID commit in the store, or `None` when no commit has been
    /// recorded yet.
    fn latest_commit(&self) -> impl Future<Output = Result<Option<Commit>, Self::Error>> + Send;

    /// Visit effective rows of one commit: exactly one row per
    /// coordinate known at `commit` (the row introduced at it or at one
    /// of its ancestors, the highest commit ID winning), including
    /// tombstones. Return `false` to stop early.
    ///
    /// The effective set is materialized before the visitor runs, so the
    /// early return saves iteration, not the read. Resolving it costs
    /// the ancestry of the commit: a caller that walks many commits
    /// pays for the history depth per commit.
    fn visit_commit_chunks<F>(
        &self,
        commit: CommitId,
        visit: F,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        F: FnMut(&ChunkHistoryEntry) -> bool + Send;

    /// Visit the rows whose `commit_id` is exactly `commit` (fresh ingests
    /// plus tombstones, no ancestor fallback). Return `false` to stop early.
    fn visit_fresh_rows<F>(
        &self,
        commit: CommitId,
        visit: F,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        F: FnMut(&ChunkHistoryEntry) -> bool + Send;

    /// Visit commits in ID order. Return `false` to stop early.
    fn visit_commits<F>(&self, visit: F) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        F: FnMut(&Commit) -> bool + Send;

    /// Load every stored region fingerprint (for skip decisions in commit).
    fn load_region_states(
        &self,
    ) -> impl Future<Output = Result<Vec<RegionStateEntry>, Self::Error>> + Send;

    /// Record a commit and optionally carry unchanged regions.
    ///
    /// The new commit continues `parents`; an empty list starts a
    /// history. As in [`Self::create_commit`], every parent must
    /// exist, a commit may not name itself, and a parent listed twice
    /// is recorded once. `entries` contains fresh rows and tombstones.
    /// `carry_from` identifies unchanged regions: backends advance their
    /// commit state and count their effective chunks, but write no
    /// per-chunk rows for them (delta storage). Fingerprints are
    /// refreshed atomically, and `removed` names regions whose state
    /// rows drop: files gone from disk or ingested only partly.
    ///
    /// Referenced blobs must already be flushed to CAS.
    ///
    /// The outcome names the commit just recorded and how many chunks it
    /// carries over from `carry_from` (the effective chunks of the carried
    /// regions, not the fresh rows in `entries`).
    fn apply_commit_incremental(
        &mut self,
        created_at_ms: u64,
        parents: &[CommitId],
        entries: &[CommitEntry],
        carry_from: Option<(CommitId, &[RegionKey])>,
        fingerprints: &[RegionFingerprint],
        removed: &[RegionKey],
    ) -> impl Future<Output = Result<ApplyOutcome, Self::Error>> + Send;

    /// Point `name` at `commit`. Duplicate names are a backend error;
    /// callers check-then-insert when they need friendlier failures.
    fn tag_commit(
        &mut self,
        name: &TagName,
        commit: CommitId,
        created_at_ms: u64,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Remove `name`. Returns whether a tag was removed.
    fn untag(&mut self, name: &TagName) -> impl Future<Output = Result<bool, Self::Error>> + Send;

    /// Tag record for `name`, or `None` when it does not exist.
    fn lookup_tag(
        &self,
        name: &TagName,
    ) -> impl Future<Output = Result<Option<CommitTag>, Self::Error>> + Send;

    /// Visit tags in name order. Return `false` to stop early.
    fn visit_tags<F>(&self, visit: F) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        F: FnMut(&CommitTag) -> bool + Send;

    /// Retire `from` into `into` (`into` must be retained and newer):
    /// rows superseded along the history `into` continues are removed,
    /// surviving rows are re-stamped onto `into`, derived states follow,
    /// and the commit row (plus its tags, by cascade) is deleted - all
    /// atomically. The children of `from` continue the history of its
    /// own parents, so the graph stays connected. Effective states of
    /// retained commits never change.
    ///
    /// `into` must be a descendant of `from`: folding into an unrelated
    /// history would graft rows the successor never had.
    fn retire_commit(
        &mut self,
        from: CommitId,
        into: CommitId,
    ) -> impl Future<Output = Result<FoldOutcome, Self::Error>> + Send;
}
