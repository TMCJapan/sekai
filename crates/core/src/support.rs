//! In-memory port fakes used by tests.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;
use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use crate::port::MetaStore;
use crate::{DimNode, RegionNode, RootNode, TreeError, TreeNodes};
use sekai_util::{
    ApplyOutcome, ChunkCoord, Commit, CommitEntry, CommitId, CommitRef, DimHash, NamedScope,
    RefName, RegionHash, RootHash, ScopeName,
};

/// Fake backend failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemError {
    /// A named parent does not exist.
    UnknownParent {
        /// Parent that was asked for.
        id: CommitId,
    },
    /// A ref would name a commit the store does not hold.
    UnknownCommit {
        /// Commit that was asked for.
        id: CommitId,
    },
    /// A tree node named by a parent is absent.
    MissingTree {
        /// Tree level.
        level: &'static str,
    },
    /// Stored tree bytes failed their canonical check.
    CorruptTree(TreeError),
    /// The ref does not exist.
    UnknownRef {
        /// Ref that was asked for.
        name: RefName,
    },
    /// The store has no active ref to hang a new commit from.
    NoActiveRef,
    /// The new commit does not continue the active ref's target.
    NotContinuing {
        /// Target the new commit had to include as a parent.
        base: CommitId,
    },
    /// The active ref cannot be removed.
    ActiveRefProtected {
        /// Ref that was asked for.
        name: RefName,
    },
}

/// Drive an immediately-ready future without an executor.
///
/// Only valid for futures that never pend (like the fakes below); a
/// pending future would spin here instead of being woken.
pub fn block_on<F: Future>(fut: F) -> F::Output {
    const VTABLE: RawWakerVTable = RawWakerVTable::new(|_| RAW, |_| {}, |_| {}, |_| {});
    const RAW: RawWaker = RawWaker::new(core::ptr::null(), &VTABLE);
    // Safety: the vtable performs no allocation and ignores its pointer,
    // so clone/wake/drop on the shared static are sound no-ops.
    let waker = unsafe { Waker::from_raw(RAW) };
    let mut cx = Context::from_waker(&waker);
    let mut fut = pin!(fut);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(out) => return out,
            Poll::Pending => core::hint::spin_loop(),
        }
    }
}

/// In-memory [`MetaStore`].
#[derive(Debug, Default)]
pub struct MemMeta {
    /// Commits in local record order (oldest first).
    pub commits: Vec<Commit>,
    /// Root nodes by hash.
    pub root_nodes: BTreeMap<RootHash, RootNode>,
    /// Dimension nodes by hash.
    pub dim_nodes: BTreeMap<DimHash, DimNode>,
    /// Region nodes by hash.
    pub region_nodes: BTreeMap<RegionHash, RegionNode>,
    /// Refs by name.
    pub refs: BTreeMap<RefName, CommitRef>,
    /// Named scopes by name.
    pub scopes: BTreeMap<ScopeName, NamedScope>,
    /// Stable local sequence per commit hash.
    pub seqs: BTreeMap<CommitId, u64>,
    /// Next local sequence number to assign.
    next_seq: u64,
}

impl MemMeta {
    fn seq_of(&self, id: CommitId) -> Option<u64> {
        self.seqs.get(&id).copied()
    }

    /// Assign a local sequence and append the commit. Returns `false`
    /// when the content-addressed ID already exists.
    fn insert_commit(
        &mut self,
        id: CommitId,
        created_at_ms: u64,
        parents: Vec<CommitId>,
        root: RootHash,
    ) -> bool {
        if self.seqs.contains_key(&id) {
            return false;
        }
        self.next_seq += 1;
        self.seqs.insert(id, self.next_seq);
        self.commits
            .push(Commit::new(id, created_at_ms, parents, root));
        true
    }

    fn insert_nodes(&mut self, nodes: &TreeNodes) -> Result<(), MemError> {
        for (hash, bytes) in &nodes.region {
            let node = RegionNode::decode(bytes, *hash).map_err(MemError::CorruptTree)?;
            self.region_nodes.insert(*hash, node);
        }
        for (hash, bytes) in &nodes.dim {
            let node = DimNode::decode(bytes, *hash).map_err(MemError::CorruptTree)?;
            self.dim_nodes.insert(*hash, node);
        }
        for (hash, bytes) in &nodes.root {
            let node = RootNode::decode(bytes, *hash).map_err(MemError::CorruptTree)?;
            self.root_nodes.insert(*hash, node);
        }
        Ok(())
    }

    /// Add an active ref, the way a fresh store looks: one ref, unborn,
    /// waiting for the first commit.
    ///
    /// Any previously active ref is deactivated: the fake keeps the same
    /// single-active-ref invariant as the backend.
    pub fn with_active_ref(&mut self, name: &RefName) {
        for (ref_name, record) in &mut self.refs {
            record.active = ref_name == name;
        }
        self.refs
            .entry(name.clone())
            .and_modify(|record| record.active = true)
            .or_insert_with(|| CommitRef::new(name.clone(), None, true, 0));
    }
}

impl MetaStore for MemMeta {
    type Error = MemError;

    fn create_commit(
        &mut self,
        created_at_ms: u64,
        parents: &[CommitId],
    ) -> impl Future<Output = Result<CommitId, MemError>> + Send {
        // Mirrors the backend: a parent must exist and a parent listed
        // twice is recorded once, in canonical hash order.
        let mut unique: BTreeSet<CommitId> = BTreeSet::new();
        for parent in parents {
            if !self.commits.iter().any(|commit| commit.id == *parent) {
                return core::future::ready(Err(MemError::UnknownParent { id: *parent }));
            }
            unique.insert(*parent);
        }
        let canonical: Vec<CommitId> = unique.into_iter().collect();
        // No snapshot changes: continue the lowest-hash parent's tree, or
        // start with the canonical empty root.
        let (root, nodes) = canonical
            .first()
            .map_or_else(crate::tree::empty_tree, |parent| {
                self.commits
                    .iter()
                    .find(|commit| commit.id == *parent)
                    .map_or_else(crate::tree::empty_tree, |commit| {
                        (commit.root, TreeNodes::default())
                    })
            });
        let id = crate::commit::commit_hash(&canonical, created_at_ms, root);
        if let Err(err) = self.insert_nodes(&nodes) {
            return core::future::ready(Err(err));
        }
        self.insert_commit(id, created_at_ms, canonical, root);
        core::future::ready(Ok(id))
    }

    fn commit_parents(
        &self,
        commit: CommitId,
    ) -> impl Future<Output = Result<Vec<CommitId>, MemError>> + Send {
        let parents = self
            .commits
            .iter()
            .find(|c| c.id == commit)
            .map_or_default(|c| c.parents.clone());
        core::future::ready(Ok(parents))
    }

    fn lookup_chunk(
        &self,
        commit: CommitId,
        coord: &ChunkCoord,
    ) -> impl Future<Output = Result<Option<CommitEntry>, MemError>> + Send {
        crate::tree::lookup_chunk(self, commit, coord)
    }

    fn visit_commit_chunks<F>(
        &self,
        commit: CommitId,
        visit: F,
    ) -> impl Future<Output = Result<(), MemError>> + Send
    where
        F: FnMut(&CommitEntry) -> bool + Send,
    {
        crate::tree::visit_chunks(self, commit, visit)
    }

    fn apply_commit_incremental(
        &mut self,
        created_at_ms: u64,
        parents: &[CommitId],
        entries: &[CommitEntry],
    ) -> impl Future<Output = Result<ApplyOutcome, MemError>> + Send {
        crate::tree::apply_entries(self, created_at_ms, parents, entries)
    }

    fn lookup_commit(
        &self,
        id: CommitId,
    ) -> impl Future<Output = Result<Option<Commit>, MemError>> + Send {
        core::future::ready(Ok(self.commits.iter().find(|c| c.id == id).cloned()))
    }

    fn latest_commit(&self) -> impl Future<Output = Result<Option<Commit>, MemError>> + Send {
        debug_assert!(
            self.commits
                .iter()
                .all(|commit| self.seqs.contains_key(&commit.id)),
            "every recorded commit has a local sequence"
        );
        let latest = self
            .commits
            .iter()
            .max_by_key(|commit| self.seq_of(commit.id))
            .cloned();
        core::future::ready(Ok(latest))
    }

    fn visit_commits<F>(&self, mut visit: F) -> impl Future<Output = Result<(), MemError>> + Send
    where
        F: FnMut(&Commit) -> bool + Send,
    {
        let mut ordered: Vec<&Commit> = self.commits.iter().collect();
        ordered.sort_by_key(|commit| self.seq_of(commit.id));
        for commit in ordered {
            if !visit(commit) {
                break;
            }
        }
        core::future::ready(Ok(()))
    }

    fn read_root_node(
        &self,
        hash: RootHash,
    ) -> impl Future<Output = Result<RootNode, MemError>> + Send {
        let node = self
            .root_nodes
            .get(&hash)
            .cloned()
            .ok_or(MemError::MissingTree { level: "root" });
        core::future::ready(node)
    }

    fn read_dim_node(
        &self,
        hash: DimHash,
    ) -> impl Future<Output = Result<DimNode, MemError>> + Send {
        let node = self
            .dim_nodes
            .get(&hash)
            .cloned()
            .ok_or(MemError::MissingTree { level: "dim" });
        core::future::ready(node)
    }

    fn read_region_node(
        &self,
        hash: RegionHash,
    ) -> impl Future<Output = Result<RegionNode, MemError>> + Send {
        let node = self
            .region_nodes
            .get(&hash)
            .cloned()
            .ok_or(MemError::MissingTree { level: "region" });
        core::future::ready(node)
    }

    fn read_dim_nodes(
        &self,
        hashes: &[DimHash],
    ) -> impl Future<Output = Result<Vec<DimNode>, MemError>> + Send {
        let nodes = hashes
            .iter()
            .map(|hash| {
                self.dim_nodes
                    .get(hash)
                    .cloned()
                    .ok_or(MemError::MissingTree { level: "dim" })
            })
            .collect();
        core::future::ready(nodes)
    }

    fn read_region_nodes(
        &self,
        hashes: &[RegionHash],
    ) -> impl Future<Output = Result<Vec<RegionNode>, MemError>> + Send {
        let nodes = hashes
            .iter()
            .map(|hash| {
                self.region_nodes
                    .get(hash)
                    .cloned()
                    .ok_or(MemError::MissingTree { level: "region" })
            })
            .collect();
        core::future::ready(nodes)
    }

    fn commit_tree(
        &mut self,
        created_at_ms: u64,
        parents: &[CommitId],
        root: RootHash,
        nodes: &TreeNodes,
    ) -> impl Future<Output = Result<ApplyOutcome, MemError>> + Send {
        // The active ref decides where the commit lands: bind it before
        // any write, so a store without one changes nothing (the backend
        // does the same in one transaction and rolls back on failure).
        let Some((active, mut record)) = self
            .refs
            .iter()
            .find(|(_, record)| record.active)
            .map(|(name, record)| (name.clone(), record.clone()))
        else {
            return core::future::ready(Err(MemError::NoActiveRef));
        };
        debug_assert!(
            self.refs.values().filter(|record| record.active).count() <= 1,
            "fake store holds more than one active ref"
        );
        // A born ref moves along its own history: unrelated parents would
        // fork it away from its target.
        if let Some(base) = record.commit
            && !parents.contains(&base)
        {
            return core::future::ready(Err(MemError::NotContinuing { base }));
        }
        let mut unique: BTreeSet<CommitId> = BTreeSet::new();
        for parent in parents {
            if !self.commits.iter().any(|commit| commit.id == *parent) {
                return core::future::ready(Err(MemError::UnknownParent { id: *parent }));
            }
            unique.insert(*parent);
        }
        let canonical: Vec<CommitId> = unique.into_iter().collect();
        let id = crate::commit::commit_hash(&canonical, created_at_ms, root);
        if let Err(err) = self.insert_nodes(nodes) {
            return core::future::ready(Err(err));
        }
        self.insert_commit(id, created_at_ms, canonical, root);
        // The active ref moves onto the new commit, in the same step: a
        // ref that lags behind its history would fork the next commit.
        record.commit = Some(id);
        self.refs.insert(active, record);
        core::future::ready(Ok(ApplyOutcome { id }))
    }

    fn set_ref(
        &mut self,
        name: &RefName,
        commit: Option<CommitId>,
        created_at_ms: u64,
    ) -> impl Future<Output = Result<(), MemError>> + Send {
        // Mirrors the backend: a ref may not name a commit the store lacks.
        if let Some(id) = commit
            && !self.commits.iter().any(|held| held.id == id)
        {
            return core::future::ready(Err(MemError::UnknownCommit { id }));
        }
        let (active, created_at_ms) = self
            .refs
            .get(name)
            .map_or((false, created_at_ms), |existing| {
                (existing.active, existing.created_at_ms)
            });
        self.refs.insert(
            name.clone(),
            CommitRef::new(name.clone(), commit, active, created_at_ms),
        );
        core::future::ready(Ok(()))
    }

    fn lookup_ref(
        &self,
        name: &RefName,
    ) -> impl Future<Output = Result<Option<CommitRef>, MemError>> + Send {
        core::future::ready(Ok(self.refs.get(name).cloned()))
    }

    fn visit_refs<F>(&self, mut visit: F) -> impl Future<Output = Result<(), MemError>> + Send
    where
        F: FnMut(&CommitRef) -> bool + Send,
    {
        for record in self.refs.values() {
            if !visit(record) {
                break;
            }
        }
        core::future::ready(Ok(()))
    }

    fn delete_ref(
        &mut self,
        name: &RefName,
    ) -> impl Future<Output = Result<bool, MemError>> + Send {
        if self.refs.get(name).is_some_and(|record| record.active) {
            return core::future::ready(Err(MemError::ActiveRefProtected { name: name.clone() }));
        }
        core::future::ready(Ok(self.refs.remove(name).is_some()))
    }

    fn active_ref(&self) -> impl Future<Output = Result<Option<CommitRef>, MemError>> + Send {
        let active = self.refs.values().find(|record| record.active).cloned();
        // Mirrors the backend: the target commit must exist.
        if let Some(record) = &active
            && let Some(commit) = record.commit
            && !self.commits.iter().any(|held| held.id == commit)
        {
            return core::future::ready(Err(MemError::UnknownCommit { id: commit }));
        }
        core::future::ready(Ok(active))
    }

    fn activate_ref(
        &mut self,
        name: &RefName,
    ) -> impl Future<Output = Result<(), MemError>> + Send {
        let Some(record) = self.refs.get(name) else {
            return core::future::ready(Err(MemError::UnknownRef { name: name.clone() }));
        };
        if let Some(commit) = record.commit
            && !self.commits.iter().any(|held| held.id == commit)
        {
            return core::future::ready(Err(MemError::UnknownCommit { id: commit }));
        }
        for record in self.refs.values_mut() {
            record.active = record.name == *name;
        }
        core::future::ready(Ok(()))
    }

    fn set_scope(
        &mut self,
        scope: &NamedScope,
    ) -> impl Future<Output = Result<(), MemError>> + Send {
        let mut record = scope.clone();
        if let Some(existing) = self.scopes.get(&scope.name) {
            record.created_at_ms = existing.created_at_ms;
        }
        self.scopes.insert(scope.name.clone(), record);
        core::future::ready(Ok(()))
    }

    fn lookup_scope(
        &self,
        name: &ScopeName,
    ) -> impl Future<Output = Result<Option<NamedScope>, MemError>> + Send {
        core::future::ready(Ok(self.scopes.get(name).cloned()))
    }

    fn visit_scopes<F>(&self, mut visit: F) -> impl Future<Output = Result<(), MemError>> + Send
    where
        F: FnMut(&NamedScope) -> bool + Send,
    {
        for scope in self.scopes.values() {
            if !visit(scope) {
                break;
            }
        }
        core::future::ready(Ok(()))
    }

    fn delete_scope(
        &mut self,
        name: &ScopeName,
    ) -> impl Future<Output = Result<bool, MemError>> + Send {
        core::future::ready(Ok(self.scopes.remove(name).is_some()))
    }
}
