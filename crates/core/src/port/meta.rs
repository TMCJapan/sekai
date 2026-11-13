//! Commit metadata and snapshot-tree storage.
//!
//! Commits form a history graph: every commit names the commits it
//! continues (`parents`) and one content-addressed root snapshot tree.
//! Backends store each tree level as opaque canonical bytes; the codec lives
//! in [`crate::tree`]. Reads follow root -> dim -> region -> CAS leaf.

use alloc::vec::Vec;
use core::future::Future;
use sekai_util::{
    ApplyOutcome, ChunkCoord, Commit, CommitEntry, CommitId, CommitRef, DimHash, NamedScope,
    RefName, RegionHash, RootHash, ScopeName,
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
    /// An unknown commit yields an empty list: the port reports what is
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

    /// Record a commit whose root tree and newly written nodes are already
    /// built.
    ///
    /// Nodes are inserted children-first, child before parent, in the same
    /// transaction as the commit row. Unchanged subtrees are shared by hash
    /// and not re-inserted. The new commit continues `parents`; an empty
    /// list starts a history. Recording a commit whose hash already exists
    /// is idempotent: the existing ID is returned, while the active ref
    /// still moves onto that commit.
    ///
    /// The new commit must continue the active ref's target: when the ref is
    /// born, `parents` includes its commit, and backends reject a mismatch
    /// instead of silently forking the ref onto another history. The active
    /// ref moves onto the new commit in the same transaction. A store
    /// without an active ref has nowhere to put the commit and fails.
    ///
    /// Referenced blobs must already be flushed to CAS.
    fn commit_tree(
        &mut self,
        created_at_ms: u64,
        parents: &[CommitId],
        root: RootHash,
        nodes: &TreeNodes,
    ) -> impl Future<Output = Result<ApplyOutcome, Self::Error>> + Send;

    /// Apply fresh entries to the parent snapshot, insert the resulting
    /// nodes, and record the commit atomically.
    fn apply_commit_incremental(
        &mut self,
        created_at_ms: u64,
        parents: &[CommitId],
        entries: &[CommitEntry],
    ) -> impl Future<Output = Result<ApplyOutcome, Self::Error>> + Send;

    /// Point `name` at `commit`, creating the ref when it does not exist.
    /// `commit = None` leaves the ref unborn: it names a history without a
    /// commit, and the next commit starts a new root from it.
    ///
    /// A new ref starts inactive; moving an existing one leaves its flag and
    /// creation time untouched. `created_at_ms` is stored only when the ref
    /// is first created.
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

    /// Visit refs in name order.
    fn visit_refs<F>(&self, visit: F) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        F: FnMut(&CommitRef) -> bool + Send;

    /// Remove `name`. Returns whether a ref was removed.
    ///
    /// The active ref is never removed.
    fn delete_ref(
        &mut self,
        name: &RefName,
    ) -> impl Future<Output = Result<bool, Self::Error>> + Send;

    /// The active ref - the one new commits hang from - or `None` when the
    /// store has none.
    fn active_ref(&self) -> impl Future<Output = Result<Option<CommitRef>, Self::Error>> + Send;

    /// Make `name` the active ref. The name and its non-`None` target must
    /// exist.
    fn activate_ref(
        &mut self,
        name: &RefName,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

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
