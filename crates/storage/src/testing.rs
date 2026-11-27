//! In-memory [`MetaStore`] fake for unit tests.
//!
//! Compiled only for this crate's own tests and for downstream crates
//! that enable the `testing` feature; a normal build carries no store
//! implementation here.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;
use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use sekai_util::{
    ApplyOutcome, Commit, CommitEntry, CommitId, CommitRef, DimHash, NamedScope, Ref, RefName,
    RegionHash, RootHash, ScopeName, chunk_coord::ChunkCoord,
};

use crate::meta::{MetaStore, commit_hash};
use crate::tree::{DimNode, RegionNode, RootNode, TreeError, TreeNodes};

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
        name: Ref,
    },
    /// The store has no active ref to hang a new commit from.
    NoActiveRef,
    /// The new commit does not continue the active ref's target.
    NotContinuing {
        /// Target the new commit had to include as a parent.
        base: CommitId,
    },
    /// The ref already exists.
    RefExists {
        /// Ref that was asked for.
        name: Ref,
    },
    /// The active ref cannot be removed or moved by a ref operation.
    ActiveRefProtected {
        /// Ref that was asked for.
        name: Ref,
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
    commits: Vec<Commit>,
    /// Root nodes by hash.
    root_nodes: BTreeMap<RootHash, RootNode>,
    /// Dimension nodes by hash.
    dim_nodes: BTreeMap<DimHash, DimNode>,
    /// Region nodes by hash.
    pub(crate) region_nodes: BTreeMap<RegionHash, RegionNode>,
    /// Refs by full refname order.
    pub(crate) refs: BTreeMap<Ref, CommitRef>,
    /// Named scopes by name.
    scopes: BTreeMap<ScopeName, NamedScope>,
    /// Stable local sequence per commit hash.
    seqs: BTreeMap<CommitId, u64>,
    /// Next local sequence number to assign.
    next_seq: u64,
}

impl MemMeta {
    fn seq_of(&self, id: CommitId) -> Option<u64> {
        self.seqs.get(&id).copied()
    }

    fn holds_commit(&self, id: CommitId) -> bool {
        self.commits.iter().any(|held| held.id == id)
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
        let name = &Ref::branch(name.clone());
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
        let id = commit_hash(&canonical, created_at_ms, root);
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
        let id = commit_hash(&canonical, created_at_ms, root);
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

    fn create_ref(
        &mut self,
        name: &Ref,
        commit: CommitId,
        created_at_ms: u64,
    ) -> impl Future<Output = Result<(), MemError>> + Send {
        // Mirrors the backend: the commit is checked before the name.
        if !self.holds_commit(commit) {
            return core::future::ready(Err(MemError::UnknownCommit { id: commit }));
        }
        if self.refs.contains_key(name) {
            return core::future::ready(Err(MemError::RefExists { name: name.clone() }));
        }
        self.refs.insert(
            name.clone(),
            CommitRef::new(name.clone(), Some(commit), false, created_at_ms),
        );
        core::future::ready(Ok(()))
    }

    fn move_ref(
        &mut self,
        name: &Ref,
        commit: CommitId,
    ) -> impl Future<Output = Result<(), MemError>> + Send {
        // Mirrors the backend: the commit is checked before the name.
        if !self.holds_commit(commit) {
            return core::future::ready(Err(MemError::UnknownCommit { id: commit }));
        }
        let result = match self.refs.get_mut(name) {
            None => Err(MemError::UnknownRef { name: name.clone() }),
            Some(record) if record.active => {
                Err(MemError::ActiveRefProtected { name: name.clone() })
            }
            Some(record) => {
                record.commit = Some(commit);
                Ok(())
            }
        };
        core::future::ready(result)
    }

    fn lookup_ref(
        &self,
        name: &Ref,
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

    fn delete_ref(&mut self, name: &Ref) -> impl Future<Output = Result<bool, MemError>> + Send {
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

    fn activate_ref(&mut self, name: &Ref) -> impl Future<Output = Result<(), MemError>> + Send {
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

#[cfg(test)]
mod tests {
    use super::*;
    use sekai_util::{ChunkHash, Dimension, RegionKind};

    const OVER: Dimension = Dimension::OVERWORLD;
    const REGION: RegionKind = RegionKind::Region;

    const fn coord(x: i32, z: i32) -> ChunkCoord {
        ChunkCoord::new(OVER, REGION, x, z)
    }

    fn present(x: i32, z: i32, byte: u8) -> CommitEntry {
        CommitEntry::new(coord(x, z), Some(ChunkHash::new([byte; 32])))
    }

    fn store_with_ref() -> MemMeta {
        let mut meta = MemMeta::default();
        meta.with_active_ref(&RefName::parse("main").unwrap());
        meta
    }

    #[test]
    fn a_commit_must_continue_the_active_ref() {
        let mut meta = store_with_ref();
        let first = block_on(meta.apply_commit_incremental(1_000, &[], &[present(0, 0, 1)]))
            .unwrap()
            .id;

        // An empty parent list on a born ref would move the active ref
        // into an unrelated history.
        let err =
            block_on(meta.apply_commit_incremental(2_000, &[], &[present(1, 0, 2)])).unwrap_err();
        assert_eq!(err, MemError::NotContinuing { base: first });

        // The rejected call wrote nothing.
        let second = block_on(meta.apply_commit_incremental(3_000, &[first], &[present(1, 0, 2)]))
            .unwrap()
            .id;
        assert_ne!(second, first);
        assert_eq!(
            block_on(meta.lookup_ref(&Ref::branch(RefName::parse("main").unwrap())))
                .unwrap()
                .unwrap()
                .commit,
            Some(second)
        );
    }

    #[test]
    fn mem_meta_satisfies_lookup_contract() {
        let mut meta = store_with_ref();
        let id = block_on(meta.apply_commit_incremental(5, &[], &[present(0, 0, 3)]))
            .unwrap()
            .id;
        let row = block_on(meta.lookup_chunk(id, &coord(0, 0)))
            .unwrap()
            .unwrap();
        assert_eq!(row, present(0, 0, 3));
        assert!(
            block_on(meta.lookup_commit(CommitId([0xFF; 32])))
                .unwrap()
                .is_none()
        );
    }

    fn branch(raw: &str) -> Ref {
        Ref::branch(RefName::parse(raw).unwrap())
    }

    #[test]
    fn create_ref_refuses_a_taken_name_or_a_missing_commit() {
        let mut meta = store_with_ref();
        let root = block_on(meta.create_commit(1_000, &[])).unwrap();
        let missing = CommitId([0xEE; 32]);

        assert_eq!(
            block_on(meta.create_ref(&branch("dev"), missing, 0)),
            Err(MemError::UnknownCommit { id: missing })
        );
        // The unborn active ref has a row, so its name is taken.
        assert_eq!(
            block_on(meta.create_ref(&branch("main"), root, 0)),
            Err(MemError::RefExists {
                name: branch("main")
            })
        );

        block_on(meta.create_ref(&branch("dev"), root, 7)).unwrap();
        let created = block_on(meta.lookup_ref(&branch("dev"))).unwrap().unwrap();
        assert_eq!(created, CommitRef::new(branch("dev"), Some(root), false, 7));
    }

    #[test]
    fn move_ref_moves_only_an_existing_inactive_ref() {
        let mut meta = store_with_ref();
        let first = block_on(meta.create_commit(1_000, &[])).unwrap();
        let second = block_on(meta.create_commit(2_000, &[first])).unwrap();
        block_on(meta.create_ref(&branch("dev"), first, 7)).unwrap();

        block_on(meta.move_ref(&branch("dev"), second)).unwrap();
        assert_eq!(
            block_on(meta.lookup_ref(&branch("dev"))).unwrap().unwrap(),
            CommitRef::new(branch("dev"), Some(second), false, 7)
        );
        assert_eq!(
            block_on(meta.move_ref(&branch("gone"), first)),
            Err(MemError::UnknownRef {
                name: branch("gone")
            })
        );
        assert_eq!(
            block_on(meta.move_ref(&branch("main"), first)),
            Err(MemError::ActiveRefProtected {
                name: branch("main")
            })
        );
        assert_eq!(
            block_on(meta.lookup_ref(&branch("main")))
                .unwrap()
                .unwrap()
                .commit,
            None
        );
    }
}
