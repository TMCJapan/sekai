//! Commit metadata and snapshot-tree storage.
//!
//! Commits form a history graph: every commit names the commits it
//! continues (`parents`) and one content-addressed root snapshot tree.
//! Backends store each tree level as opaque canonical bytes; the codec lives
//! in [`crate::tree`]. Reads follow root -> dim -> region -> CAS leaf.

use alloc::vec::Vec;
use core::future::Future;
use sekai_util::{
    ApplyOutcome, ChunkCoord, Commit, CommitEntry, CommitId, CommitRef, DimHash, Head, HeadState,
    NamedScope, Ref, RefName, ReflogEntry, ReflogNote, RegionHash, RootHash, ScopeName, Worktree,
    WorktreeName,
};

use crate::{DimNode, RegionNode, RootNode, TreeNodes};

/// Commit metadata and snapshot-tree storage.
pub trait MetaStore {
    /// Backend failure (query errors, constraint violations, ...).
    type Error;

    /// Append a commit continuing `parents` with no snapshot changes; an empty
    /// parent list starts a history with the empty tree.
    ///
    /// The ID is derived from the commit's canonical content, so recording
    /// the same logical commit twice returns the existing ID instead of
    /// adding a row. Every parent must exist and a parent listed twice is
    /// recorded once.
    fn create_commit(
        &mut self,
        created_at_ms: u64,
        parents: &[CommitId],
    ) -> impl Future<Output = Result<CommitId, Self::Error>> + Send;

    /// Direct parents of `commit`; empty for the root of a history.
    ///
    /// An unknown commit yields an empty list: the trait reports what is
    /// stored, not what exists. The use cases above it check for existence
    /// first.
    fn commit_parents(
        &self,
        commit: CommitId,
    ) -> impl Future<Output = Result<Vec<CommitId>, Self::Error>> + Send;

    /// Effective present entry for (`commit`, `coord`), or `None` when the
    /// snapshot has no chunk there. Absence is the tombstone.
    fn lookup_chunk(
        &self,
        commit: CommitId,
        coord: &ChunkCoord,
    ) -> impl Future<Output = Result<Option<CommitEntry>, Self::Error>> + Send;

    /// Commit metadata for `id`, or `None` when it does not exist.
    fn lookup_commit(
        &self,
        id: CommitId,
    ) -> impl Future<Output = Result<Option<Commit>, Self::Error>> + Send;

    /// Most recently recorded commit in the store, or `None` when no
    /// commit has been recorded yet.
    fn latest_commit(&self) -> impl Future<Output = Result<Option<Commit>, Self::Error>> + Send;

    /// Visit every present chunk of one commit's snapshot, in global
    /// coordinate order. Absent coordinates are never visited. Return
    /// `false` to stop early.
    ///
    /// The snapshot is materialized before the visitor runs, so the early
    /// return saves iteration, not the read: the walk costs the whole tree
    /// no matter where the visitor stops.
    fn visit_commit_chunks<F>(
        &self,
        commit: CommitId,
        visit: F,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        F: FnMut(&CommitEntry) -> bool + Send;

    /// Visit commits in local record order (oldest first). Return `false`
    /// to stop early.
    fn visit_commits<F>(&self, visit: F) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        F: FnMut(&Commit) -> bool + Send;

    /// Read and verify one root node. A missing or corrupt node is a
    /// backend error, never an empty tree.
    fn read_root_node(
        &self,
        hash: RootHash,
    ) -> impl Future<Output = Result<RootNode, Self::Error>> + Send;

    /// Read and verify one dimension node.
    fn read_dim_node(
        &self,
        hash: DimHash,
    ) -> impl Future<Output = Result<DimNode, Self::Error>> + Send;

    /// Read and verify one region node.
    fn read_region_node(
        &self,
        hash: RegionHash,
    ) -> impl Future<Output = Result<RegionNode, Self::Error>> + Send;

    /// Read and verify several dimension nodes in one backend read.
    ///
    /// Returns one node per input hash, in input order; a repeated hash
    /// yields a repeated node. A missing or corrupt node is a backend
    /// error, never an empty tree. An empty input reads nothing.
    fn read_dim_nodes(
        &self,
        hashes: &[DimHash],
    ) -> impl Future<Output = Result<Vec<DimNode>, Self::Error>> + Send;

    /// Read and verify several region nodes in one backend read.
    ///
    /// Returns one node per input hash, in input order; a repeated hash
    /// yields a repeated node. A missing or corrupt node is a backend
    /// error, never an empty tree. An empty input reads nothing.
    fn read_region_nodes(
        &self,
        hashes: &[RegionHash],
    ) -> impl Future<Output = Result<Vec<RegionNode>, Self::Error>> + Send;

    /// Record a commit on `worktree`'s HEAD whose root tree and newly
    /// written nodes are already built.
    ///
    /// Nodes are inserted children-first, child before parent, in the same
    /// transaction as the commit row. Unchanged subtrees are shared by hash
    /// and not re-inserted. The new commit continues `parents`; an empty
    /// list starts a history. Recording a commit whose hash already exists
    /// is idempotent: the existing ID is returned, while HEAD still moves
    /// onto that commit.
    ///
    /// When HEAD stands at a commit, `parents` must include it: backends
    /// reject a mismatch instead of silently forking HEAD onto another
    /// history. In the same transaction the commit moves HEAD's branch,
    /// creating it when HEAD is unborn, or moves a detached HEAD, and
    /// appends `note` to the branch's reflog and to HEAD's. An unknown
    /// `worktree` is refused.
    ///
    /// Referenced blobs must already be flushed to CAS.
    fn commit_tree(
        &mut self,
        worktree: &WorktreeName,
        created_at_ms: u64,
        parents: &[CommitId],
        root: RootHash,
        nodes: &TreeNodes,
        note: ReflogNote<'_>,
    ) -> impl Future<Output = Result<ApplyOutcome, Self::Error>> + Send;

    /// Apply fresh entries to the parent snapshot, insert the resulting
    /// nodes, and record the commit on `worktree`'s HEAD atomically, as
    /// [`Self::commit_tree`] does.
    fn apply_commit_incremental(
        &mut self,
        worktree: &WorktreeName,
        created_at_ms: u64,
        parents: &[CommitId],
        entries: &[CommitEntry],
        note: ReflogNote<'_>,
    ) -> impl Future<Output = Result<ApplyOutcome, Self::Error>> + Send;

    /// The ref `name`, or `None` when it does not exist.
    fn lookup_ref(
        &self,
        name: &Ref,
    ) -> impl Future<Output = Result<Option<CommitRef>, Self::Error>> + Send;

    /// Visit refs in full refname order (byte-wise). Return `false` to stop
    /// early.
    fn visit_refs<F>(&self, visit: F) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        F: FnMut(&CommitRef) -> bool + Send;

    /// Create `name` at `commit`. A branch logs its creation with `note`.
    ///
    /// Fails when the ref exists, when it would conflict with another ref
    /// or an unborn HEAD's branch as a directory and a file would, or when
    /// `commit` is unknown.
    fn create_ref(
        &mut self,
        name: &Ref,
        commit: CommitId,
        note: ReflogNote<'_>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Move the existing ref `name` to `commit`, returning where it pointed
    /// before. A branch logs the move with `note`.
    ///
    /// A branch checked out by any worktree is refused.
    fn move_ref(
        &mut self,
        name: &Ref,
        commit: CommitId,
        note: ReflogNote<'_>,
    ) -> impl Future<Output = Result<CommitId, Self::Error>> + Send;

    /// Remove `name` and, for a branch, its reflog. Returns the commit it
    /// pointed at, or `None` when no such ref exists.
    ///
    /// A branch checked out by any worktree is refused.
    fn delete_ref(
        &mut self,
        name: &Ref,
    ) -> impl Future<Output = Result<Option<CommitId>, Self::Error>> + Send;

    /// Rename branch `from` to `to`, over an existing `to` when `force`
    /// is set, as Git's `branch -m` and `branch -M` do.
    ///
    /// Every HEAD naming `from` follows the rename. A born branch keeps
    /// its reflog under the new name and appends `note` to it, and so does
    /// the reflog of every HEAD on it; a `to` it replaces is deleted with
    /// its reflog. An unborn branch, named only by a HEAD, changes only
    /// that HEAD, which attaches to a `to` that exists and logs `note`
    /// there. Renaming a born branch to itself only logs `note`, and an
    /// unborn one to itself changes nothing.
    ///
    /// Fails when `from` is neither a branch nor an unborn HEAD's branch,
    /// when `to` differs from `from` and exists without `force` or is
    /// checked out, or when `to` would conflict as a directory and a file
    /// would with any ref other than `from`.
    fn rename_branch(
        &mut self,
        from: &RefName,
        to: &RefName,
        force: bool,
        note: ReflogNote<'_>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Register `worktree`. A HEAD at a commit logs `note` as the first
    /// entry of its reflog.
    ///
    /// Fails when the name or path is taken, when a second main worktree
    /// is added, when HEAD's branch is checked out elsewhere or would
    /// conflict as a directory and a file would, or when a detached HEAD
    /// names an unknown commit.
    fn add_worktree(
        &mut self,
        worktree: &Worktree,
        note: ReflogNote<'_>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// The worktree `name`, or `None` when it does not exist.
    fn lookup_worktree(
        &self,
        name: &WorktreeName,
    ) -> impl Future<Output = Result<Option<Worktree>, Self::Error>> + Send;

    /// Visit worktrees in name order. Return `false` to stop early.
    fn visit_worktrees<F>(&self, visit: F) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        F: FnMut(&Worktree) -> bool + Send;

    /// Remove worktree `name` with its HEAD reflog. Returns whether a
    /// worktree was removed. The main worktree is refused.
    fn remove_worktree(
        &mut self,
        name: &WorktreeName,
    ) -> impl Future<Output = Result<bool, Self::Error>> + Send;

    /// Point worktree `name`'s HEAD at `head`, as a checkout does. When
    /// the new HEAD stands at a commit, its reflog records `note`.
    ///
    /// Fails for an unknown worktree, a branch checked out elsewhere or
    /// conflicting as a directory and a file would, or a detached HEAD
    /// at an unknown commit.
    fn set_head(
        &mut self,
        name: &WorktreeName,
        head: &Head,
        note: ReflogNote<'_>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Worktree `name`'s HEAD resolved against the refs, or `None` when
    /// the worktree does not exist.
    fn head(
        &self,
        name: &WorktreeName,
    ) -> impl Future<Output = Result<Option<HeadState>, Self::Error>> + Send;

    /// Visit branch `name`'s reflog, newest entry first. A branch without
    /// a reflog visits nothing. Return `false` to stop early.
    fn visit_ref_log<F>(
        &self,
        name: &RefName,
        visit: F,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        F: FnMut(&ReflogEntry) -> bool + Send;

    /// Visit worktree `name`'s HEAD reflog, newest entry first. A
    /// worktree without a reflog visits nothing. Return `false` to stop
    /// early.
    fn visit_head_log<F>(
        &self,
        name: &WorktreeName,
        visit: F,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        F: FnMut(&ReflogEntry) -> bool + Send;

    /// Store `scope`, replacing every area of an existing scope of the same
    /// name in one atomic step.
    fn set_scope(
        &mut self,
        scope: &NamedScope,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// The scope with this name, or `None` when it does not exist.
    fn lookup_scope(
        &self,
        name: &ScopeName,
    ) -> impl Future<Output = Result<Option<NamedScope>, Self::Error>> + Send;

    /// Visit scopes in name order.
    fn visit_scopes<F>(&self, visit: F) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        F: FnMut(&NamedScope) -> bool + Send;

    /// Remove `name` with all its areas. Returns whether a scope was removed.
    fn delete_scope(
        &mut self,
        name: &ScopeName,
    ) -> impl Future<Output = Result<bool, Self::Error>> + Send;
}
