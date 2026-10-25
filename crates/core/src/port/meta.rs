//! Commit metadata and per-chunk history storage.
//!
//! Commits form a history graph: every commit names the commits it
//! continues (`parents`). A store therefore answers "what did this
//! commit inherit?" by walking parent links, not by comparing
//! identifiers. Commit IDs are content-addressed hashes; backends keep a
//! local record order for traversal and tie-breaking but never expose it
//! as an ID.

use alloc::vec::Vec;
use core::future::Future;
use sekai_util::{
    ApplyOutcome, BlobHash, ChunkCoord, ChunkHistoryEntry, Commit, CommitEntry, CommitId,
    CommitRef, CommitTag, DiffHash, FoldOutcome, RefName, RegionFingerprint, RegionKey,
    RegionStateEntry, TagName,
};

/// Commit metadata and per-chunk history storage.
pub trait MetaStore {
    /// Backend failure (query errors, constraint violations, ...).
    type Error;

    /// Append a commit continuing `parents`; an empty list starts a
    /// history.
    ///
    /// The ID is derived from the commit's canonical content, so
    /// recording the same logical commit twice returns the existing ID
    /// instead of adding a row. Every parent must exist and a parent
    /// listed twice is recorded once.
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
    /// most recently recorded one wins, or `None` when no ancestor ever
    /// recorded the coordinate.
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

    /// Most recently recorded commit in the store, or `None` when no
    /// commit has been recorded yet.
    fn latest_commit(&self) -> impl Future<Output = Result<Option<Commit>, Self::Error>> + Send;

    /// Visit effective rows of one commit: exactly one row per
    /// coordinate known at `commit` (the row introduced at it or at one
    /// of its ancestors, the most recently recorded winning), including
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

    /// Visit the rows introduced at exactly `commit` (fresh ingests plus
    /// tombstones, no ancestor fallback). Return `false` to stop early.
    fn visit_fresh_rows<F>(
        &self,
        commit: CommitId,
        visit: F,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        F: FnMut(&ChunkHistoryEntry) -> bool + Send;

    /// Visit commits in local record order (oldest first). Return `false`
    /// to stop early.
    fn visit_commits<F>(&self, visit: F) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        F: FnMut(&Commit) -> bool + Send;

    /// Load every stored region fingerprint (for skip decisions in commit).
    ///
    /// `within` restricts the result to the history of that commit: a
    /// fingerprint confirmed outside it describes another history (another
    /// ref) and must not be carried into this one. "Inside" means stamped
    /// at `within` or at one of its ancestors - the same ancestry the
    /// effective chunk reads use. `None` - an unborn ref - yields nothing:
    /// its history holds no confirmed file yet.
    fn load_region_states(
        &self,
        within: Option<CommitId>,
    ) -> impl Future<Output = Result<Vec<RegionStateEntry>, Self::Error>> + Send;

    /// Record a commit and optionally carry unchanged regions.
    ///
    /// The new commit continues `parents`; an empty list starts a
    /// history. As in [`Self::create_commit`], every parent must exist
    /// and a parent listed twice is recorded once. Recording a commit
    /// whose hash already exists is idempotent: the existing ID is
    /// returned and no fresh rows are written. Shared derived state
    /// (carried region state, fingerprints, and removals), the row
    /// counts from `carry_from`, and the active-ref move are still
    /// applied to that commit, so a repeat record advances derived
    /// state and the ref while the commit graph stays unchanged.
    /// `entries` contains fresh rows and tombstones.
    /// `carry_from` identifies unchanged regions: backends advance their
    /// commit state and count their effective chunks, but write no
    /// per-chunk rows for them (delta storage). Fingerprints are
    /// refreshed atomically, and `removed` names regions whose state
    /// rows drop: files gone from disk or ingested only partly.
    ///
    /// The new commit must continue the active ref's target: when the
    /// ref is born, `parents` includes its commit, and backends reject a
    /// mismatch instead of silently forking the ref onto another
    /// history. The active ref moves onto the new commit in the same
    /// transaction, since a ref lagging behind its history would fork
    /// the next commit. A store without an active ref has nowhere to put
    /// the commit and fails.
    ///
    /// A store with no active ref fails with the backend's no-active-ref
    /// error before any parent check. An unborn active ref imposes no
    /// parent constraint: the new commit starts its history. Only a born
    /// active ref whose target is absent from `parents` fails as "not
    /// continuing".
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

    /// Retire `from` into `into` (`into` must be retained and have a
    /// larger local sequence than `from`):
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

    /// Point `name` at `commit`, creating the ref when it does not exist.
    /// `commit = None` leaves the ref unborn: it names a history without
    /// a commit, and the next commit starts a new root from it. That is
    /// the state a fresh store starts in, so moving the active ref to
    /// unborn is allowed and is not the dangling target
    /// [`Self::active_ref`] rejects: an unborn active ref reads back as
    /// a valid ref whose `commit` is `None`. A non-`None` target must name
    /// an existing commit; otherwise the backend fails with its
    /// unknown-commit error.
    ///
    /// A new ref starts inactive; moving an existing one leaves its flag
    /// and creation time untouched. `created_at_ms` is stored only when
    /// the ref is first created; an existing name keeps its original
    /// timestamp even though the argument must still be storable by the
    /// backend.
    fn set_ref(
        &mut self,
        name: &RefName,
        commit: Option<CommitId>,
        created_at_ms: u64,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// The ref with this name, or `None` when it does not exist.
    fn lookup_ref(
        &self,
        name: &RefName,
    ) -> impl Future<Output = Result<Option<CommitRef>, Self::Error>> + Send;

    /// Visit refs in name order - the order [`RefName`]'s `Ord` yields -
    /// so every backend lists them alike. Return `false` to stop early;
    /// `Ok(())` only reports that the visit ran without error. Early
    /// stopping is not signalled: a caller that needs a complete set must
    /// keep returning `true`.
    fn visit_refs<F>(&self, visit: F) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        F: FnMut(&CommitRef) -> bool + Send;

    /// Remove `name`. Returns whether a ref was removed: `false` means
    /// no ref of that name exists.
    ///
    /// The active ref is never removed: a store without one would have
    /// nowhere to hang new commits from. Asking to remove it is an
    /// error, not a `false` result. The check is part of the delete
    /// itself, so callers need not read the active ref first (and doing
    /// so races against a concurrent [`Self::activate_ref`]).
    fn delete_ref(
        &mut self,
        name: &RefName,
    ) -> impl Future<Output = Result<bool, Self::Error>> + Send;

    /// The active ref - the one new commits hang from - or `None` when
    /// the store has none.
    ///
    /// The commit it names must exist: a dangling target is corruption,
    /// not an unborn ref, and fails with the backend's unknown-commit
    /// error.
    fn active_ref(&self) -> impl Future<Output = Result<Option<CommitRef>, Self::Error>> + Send;

    /// Make `name` the active ref. The name must exist; a missing name
    /// fails with the backend's unknown-ref error. A non-`None` target
    /// must also exist: activating a ref whose target is dangling is
    /// corruption, and the backend fails with its unknown-commit error
    /// rather than making a headless store.
    ///
    /// The previous active ref is cleared before `name` is set: a store
    /// holds at most one active ref. Backends keep that order; SQLite
    /// enforces it with a partial unique index, so setting without
    /// clearing fails on the constraint.
    fn activate_ref(
        &mut self,
        name: &RefName,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}
