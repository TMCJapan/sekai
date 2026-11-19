//! In-memory `MetaStore` fake used by tests.

use alloc::borrow::ToOwned;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use sekai_storage::{DimNode, MetaStore, RegionNode, RootNode, TreeError, TreeNodes};
use sekai_util::refs::paths_conflict;
use sekai_util::{
    ApplyOutcome, ChunkCoord, Commit, CommitEntry, CommitId, CommitRef, DimHash, Head, HeadState,
    NamedScope, Ref, RefName, ReflogEntry, ReflogNote, RegionHash, RootHash, ScopeName, Worktree,
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
        name: Ref,
    },
    /// The ref already exists.
    RefExists {
        /// Ref that was asked for.
        name: Ref,
    },
    /// The ref would conflict with another as a directory and a file.
    RefConflict {
        /// Ref that was asked for.
        name: Ref,
        /// Ref or unborn branch it conflicts with.
        existing: Ref,
    },
    /// The branch is checked out by another worktree.
    BranchCheckedOut {
        /// Branch that was asked for.
        branch: RefName,
        /// Path of the worktree that has it checked out.
        worktree: String,
    },
    /// No worktree has this path.
    UnknownWorktree {
        /// Worktree path that was asked for.
        path: String,
    },
    /// Another worktree already has this path.
    WorktreePathTaken {
        /// Path that was asked for.
        path: String,
    },
    /// The store already has a main worktree.
    MainWorktreeExists,
    /// The main worktree cannot be removed or moved.
    MainWorktreeProtected {
        /// Worktree path that was asked for.
        path: String,
    },
    /// The new commit does not continue HEAD's commit.
    NotContinuing {
        /// Commit the new commit had to include as a parent.
        base: CommitId,
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
    /// Refs, in full refname order.
    pub refs: BTreeMap<Ref, CommitId>,
    /// Worktrees by world root path.
    pub worktrees: BTreeMap<String, Worktree>,
    /// Branch reflogs, oldest entry first.
    pub ref_logs: BTreeMap<RefName, Vec<ReflogEntry>>,
    /// HEAD reflogs by worktree path, oldest entry first.
    pub head_logs: BTreeMap<String, Vec<ReflogEntry>>,
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

    /// Add the main worktree at [`main_worktree`] with HEAD unborn on
    /// `main`, the way a freshly initialized world looks.
    pub fn with_main(&mut self) {
        let path = main_worktree();
        self.worktrees.insert(
            path.clone(),
            Worktree {
                path,
                main: true,
                head: Head::Branch(RefName::parse("main").unwrap()),
            },
        );
    }

    /// Commit `entries` on the main worktree's HEAD.
    pub fn apply_main(
        &mut self,
        created_at_ms: u64,
        parents: &[CommitId],
        entries: &[CommitEntry],
    ) -> impl Future<Output = Result<ApplyOutcome, MemError>> + Send {
        let outcome = block_on(self.apply_commit_incremental(
            &main_worktree(),
            created_at_ms,
            parents,
            entries,
            ReflogNote::new("commit: ", created_at_ms),
        ));
        core::future::ready(outcome)
    }

    fn has_commit(&self, id: CommitId) -> bool {
        self.seqs.contains_key(&id)
    }

    fn require_commit(&self, id: CommitId) -> Result<(), MemError> {
        if self.has_commit(id) {
            Ok(())
        } else {
            Err(MemError::UnknownCommit { id })
        }
    }

    fn head_state(&self, worktree: &Worktree) -> HeadState {
        let commit = match &worktree.head {
            Head::Branch(branch) => self.refs.get(&Ref::branch(branch.clone())).copied(),
            Head::Detached(_) => None,
        };
        worktree.head.resolve(commit)
    }

    fn checked_out_by(&self, branch: &RefName) -> Option<&String> {
        self.worktrees
            .values()
            .find(|worktree| worktree.head.branch() == Some(branch))
            .map(|worktree| &worktree.path)
    }

    /// Refuse `branch` when a worktree other than `allowed` has it
    /// checked out.
    fn check_not_checked_out(
        &self,
        branch: &RefName,
        allowed: Option<&str>,
    ) -> Result<(), MemError> {
        match self.checked_out_by(branch) {
            Some(holder) if Some(holder.as_str()) != allowed => Err(MemError::BranchCheckedOut {
                branch: branch.clone(),
                worktree: holder.clone(),
            }),
            _ => Ok(()),
        }
    }

    /// Refuse `name` when it conflicts as a directory and a file would
    /// with a ref or an unborn HEAD's branch other than `except`.
    fn check_no_conflict(&self, name: &Ref, except: Option<&Ref>) -> Result<(), MemError> {
        let full = name.full_name();
        let unborn = self
            .worktrees
            .values()
            .filter_map(|worktree| worktree.head.branch())
            .map(|branch| Ref::branch(branch.clone()));
        let conflict = self
            .refs
            .keys()
            .cloned()
            .chain(unborn)
            .filter(|other| Some(other) != except)
            .find(|other| paths_conflict(&full, &other.full_name()));
        conflict.map_or(Ok(()), |existing| {
            Err(MemError::RefConflict {
                name: name.clone(),
                existing,
            })
        })
    }

    fn log_ref(
        &mut self,
        branch: &RefName,
        old: Option<CommitId>,
        new: CommitId,
        note: ReflogNote<'_>,
    ) {
        self.ref_logs
            .entry(branch.clone())
            .or_default()
            .push(log_entry(old, new, note));
    }

    fn log_head(
        &mut self,
        worktree: &str,
        old: Option<CommitId>,
        new: CommitId,
        note: ReflogNote<'_>,
    ) {
        self.head_logs
            .entry(worktree.to_owned())
            .or_default()
            .push(log_entry(old, new, note));
    }

    fn record_commit(
        &mut self,
        path: &str,
        created_at_ms: u64,
        parents: &[CommitId],
        root: RootHash,
        nodes: &TreeNodes,
        note: ReflogNote<'_>,
    ) -> Result<ApplyOutcome, MemError> {
        // Every check runs before the first write, as the backend's single
        // transaction would roll back on failure.
        let state = self
            .worktrees
            .get(path)
            .map(|held| self.head_state(held))
            .ok_or_else(|| MemError::UnknownWorktree {
                path: path.to_owned(),
            })?;
        if let Some(base) = state.commit()
            && !parents.contains(&base)
        {
            return Err(MemError::NotContinuing { base });
        }
        if let HeadState::Unborn { branch } = &state {
            self.check_no_conflict(&Ref::branch(branch.clone()), None)?;
        }
        let mut unique: BTreeSet<CommitId> = BTreeSet::new();
        for parent in parents {
            if !self.has_commit(*parent) {
                return Err(MemError::UnknownParent { id: *parent });
            }
            unique.insert(*parent);
        }
        let canonical: Vec<CommitId> = unique.into_iter().collect();
        let id = crate::commit::commit_hash(&canonical, created_at_ms, root);
        self.insert_nodes(nodes)?;
        self.insert_commit(id, created_at_ms, canonical, root);
        match &state {
            HeadState::Attached { branch, commit } => {
                self.refs.insert(Ref::branch(branch.clone()), id);
                self.log_ref(branch, Some(*commit), id, note);
            }
            HeadState::Unborn { branch } => {
                self.refs.insert(Ref::branch(branch.clone()), id);
                self.log_ref(branch, None, id, note);
            }
            HeadState::Detached { .. } => {
                if let Some(held) = self.worktrees.get_mut(path) {
                    held.head = Head::Detached(id);
                }
            }
        }
        self.log_head(path, state.commit(), id, note);
        Ok(ApplyOutcome { id })
    }

    fn rename(
        &mut self,
        from: &RefName,
        to: &RefName,
        force: bool,
        note: ReflogNote<'_>,
    ) -> Result<(), MemError> {
        let from_ref = Ref::branch(from.clone());
        let to_ref = Ref::branch(to.clone());
        let commit = self.refs.get(&from_ref).copied();
        let holders: Vec<String> = self
            .worktrees
            .values()
            .filter(|worktree| worktree.head.branch() == Some(from))
            .map(|worktree| worktree.path.clone())
            .collect();
        if commit.is_none() && holders.is_empty() {
            return Err(MemError::UnknownRef { name: from_ref });
        }
        let mut replaced = None;
        if from != to {
            replaced = self.refs.get(&to_ref).copied();
            if replaced.is_some() && !force {
                return Err(MemError::RefExists { name: to_ref });
            }
            self.check_not_checked_out(to, None)?;
            self.check_no_conflict(&to_ref, Some(&from_ref))?;
            for holder in &holders {
                if let Some(worktree) = self.worktrees.get_mut(holder) {
                    worktree.head = Head::Branch(to.clone());
                }
            }
            if let Some(commit) = commit {
                self.refs.remove(&from_ref);
                self.refs.insert(to_ref, commit);
                let log = self.ref_logs.remove(from).unwrap_or_default();
                self.ref_logs.insert(to.clone(), log);
            }
        }
        match (commit, replaced) {
            (Some(commit), _) => {
                self.log_ref(to, Some(commit), commit, note);
                for holder in &holders {
                    self.log_head(holder, Some(commit), commit, note);
                }
            }
            // An unborn HEAD lands on the branch it was renamed over.
            (None, Some(landed)) => {
                for holder in &holders {
                    self.log_head(holder, None, landed, note);
                }
            }
            // Otherwise an unborn HEAD stays unborn and logs nothing.
            (None, None) => {}
        }
        Ok(())
    }
}

/// World root of the main worktree [`MemMeta::with_main`] adds.
pub fn main_worktree() -> String {
    String::from("/worlds/main")
}

fn log_entry(old: Option<CommitId>, new: CommitId, note: ReflogNote<'_>) -> ReflogEntry {
    ReflogEntry {
        old,
        new,
        message: note.message.to_owned(),
        at_ms: note.at_ms,
    }
}

/// Visit `log` newest entry first.
fn visit_newest_first<F>(log: Option<&Vec<ReflogEntry>>, mut visit: F)
where
    F: FnMut(&ReflogEntry) -> bool,
{
    for entry in log.into_iter().flatten().rev() {
        if !visit(entry) {
            break;
        }
    }
}

impl MemMeta {
    /// Seed a history with a commit that changes nothing, continuing the
    /// lowest-hash parent's tree. Not part of the store's contract: used
    /// by tests to build histories without writing region data.
    pub(crate) fn create_commit(
        &mut self,
        created_at_ms: u64,
        parents: &[CommitId],
    ) -> Result<CommitId, MemError> {
        // Mirrors the backend: a parent must exist and a parent listed
        // twice is recorded once, in canonical hash order.
        let mut unique: BTreeSet<CommitId> = BTreeSet::new();
        for parent in parents {
            if !self.commits.iter().any(|commit| commit.id == *parent) {
                return Err(MemError::UnknownParent { id: *parent });
            }
            unique.insert(*parent);
        }
        let canonical: Vec<CommitId> = unique.into_iter().collect();
        // No snapshot changes: continue the lowest-hash parent's tree, or
        // start with the canonical empty root.
        let (root, nodes) =
            canonical
                .first()
                .map_or_else(sekai_storage::tree::empty_tree, |parent| {
                    self.commits
                        .iter()
                        .find(|commit| commit.id == *parent)
                        .map_or_else(sekai_storage::tree::empty_tree, |commit| {
                            (commit.root, TreeNodes::default())
                        })
                });
        let id = crate::commit::commit_hash(&canonical, created_at_ms, root);
        self.insert_nodes(&nodes)?;
        self.insert_commit(id, created_at_ms, canonical, root);
        Ok(id)
    }
}

impl MetaStore for MemMeta {
    type Error = MemError;

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
        path: &str,
        created_at_ms: u64,
        parents: &[CommitId],
        entries: &[CommitEntry],
        note: ReflogNote<'_>,
    ) -> impl Future<Output = Result<ApplyOutcome, MemError>> + Send {
        crate::tree::apply_entries(self, path, created_at_ms, parents, entries, note)
    }

    fn lookup_commit(
        &self,
        id: CommitId,
    ) -> impl Future<Output = Result<Option<Commit>, MemError>> + Send {
        core::future::ready(Ok(self.commits.iter().find(|c| c.id == id).cloned()))
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
        path: &str,
        created_at_ms: u64,
        parents: &[CommitId],
        root: RootHash,
        nodes: &TreeNodes,
        note: ReflogNote<'_>,
    ) -> impl Future<Output = Result<ApplyOutcome, MemError>> + Send {
        core::future::ready(self.record_commit(path, created_at_ms, parents, root, nodes, note))
    }

    fn lookup_ref(
        &self,
        name: &Ref,
    ) -> impl Future<Output = Result<Option<CommitRef>, MemError>> + Send {
        let record = self
            .refs
            .get(name)
            .map(|&commit| CommitRef::new(name.clone(), commit));
        core::future::ready(Ok(record))
    }

    fn visit_refs<F>(&self, mut visit: F) -> impl Future<Output = Result<(), MemError>> + Send
    where
        F: FnMut(&CommitRef) -> bool + Send,
    {
        for (name, &commit) in &self.refs {
            if !visit(&CommitRef::new(name.clone(), commit)) {
                break;
            }
        }
        core::future::ready(Ok(()))
    }

    fn create_ref(
        &mut self,
        name: &Ref,
        commit: CommitId,
        note: ReflogNote<'_>,
    ) -> impl Future<Output = Result<(), MemError>> + Send {
        let result = (|| {
            if self.refs.contains_key(name) {
                return Err(MemError::RefExists { name: name.clone() });
            }
            self.check_no_conflict(name, None)?;
            self.require_commit(commit)?;
            self.refs.insert(name.clone(), commit);
            if name.kind == sekai_util::RefKind::Branch {
                self.log_ref(&name.name, None, commit, note);
                // The branch an unborn HEAD names now has a commit, which
                // leaves that HEAD at it.
                if let Some(holder) = self.checked_out_by(&name.name).cloned() {
                    self.log_head(&holder, None, commit, note);
                }
            }
            Ok(())
        })();
        core::future::ready(result)
    }

    fn move_ref(
        &mut self,
        name: &Ref,
        commit: CommitId,
        note: ReflogNote<'_>,
    ) -> impl Future<Output = Result<CommitId, MemError>> + Send {
        let result = (|| {
            let old = *self
                .refs
                .get(name)
                .ok_or_else(|| MemError::UnknownRef { name: name.clone() })?;
            self.require_commit(commit)?;
            if name.kind == sekai_util::RefKind::Tag {
                self.refs.insert(name.clone(), commit);
                return Ok(old);
            }
            self.check_not_checked_out(&name.name, None)?;
            self.refs.insert(name.clone(), commit);
            self.log_ref(&name.name, Some(old), commit, note);
            Ok(old)
        })();
        core::future::ready(result)
    }

    fn delete_ref(
        &mut self,
        name: &Ref,
    ) -> impl Future<Output = Result<Option<CommitId>, MemError>> + Send {
        let result = (|| {
            if name.kind == sekai_util::RefKind::Branch && self.refs.contains_key(name) {
                self.check_not_checked_out(&name.name, None)?;
                self.ref_logs.remove(&name.name);
            }
            Ok(self.refs.remove(name))
        })();
        core::future::ready(result)
    }

    fn rename_branch(
        &mut self,
        from: &RefName,
        to: &RefName,
        force: bool,
        note: ReflogNote<'_>,
    ) -> impl Future<Output = Result<(), MemError>> + Send {
        core::future::ready(self.rename(from, to, force, note))
    }

    fn add_worktree(
        &mut self,
        worktree: &Worktree,
        note: ReflogNote<'_>,
    ) -> impl Future<Output = Result<(), MemError>> + Send {
        let result = (|| {
            if self.worktrees.contains_key(&worktree.path) {
                return Err(MemError::WorktreePathTaken {
                    path: worktree.path.clone(),
                });
            }
            if worktree.main && self.worktrees.values().any(Worktree::is_main) {
                return Err(MemError::MainWorktreeExists);
            }
            match &worktree.head {
                Head::Branch(branch) => {
                    self.check_not_checked_out(branch, None)?;
                    self.check_no_conflict(&Ref::branch(branch.clone()), None)?;
                }
                Head::Detached(commit) => self.require_commit(*commit)?,
            }
            let state = self.head_state(worktree);
            self.worktrees
                .insert(worktree.path.clone(), worktree.clone());
            if let Some(commit) = state.commit() {
                self.log_head(&worktree.path, None, commit, note);
            }
            Ok(())
        })();
        core::future::ready(result)
    }

    fn lookup_worktree(
        &self,
        path: &str,
    ) -> impl Future<Output = Result<Option<Worktree>, MemError>> + Send {
        core::future::ready(Ok(self.worktrees.get(path).cloned()))
    }

    fn visit_worktrees<F>(&self, mut visit: F) -> impl Future<Output = Result<(), MemError>> + Send
    where
        F: FnMut(&Worktree) -> bool + Send,
    {
        for worktree in self.worktrees.values() {
            if !visit(worktree) {
                break;
            }
        }
        core::future::ready(Ok(()))
    }

    fn remove_worktree(
        &mut self,
        path: &str,
    ) -> impl Future<Output = Result<bool, MemError>> + Send {
        let result = match self.worktrees.get(path) {
            None => Ok(false),
            Some(worktree) if worktree.is_main() => Err(MemError::MainWorktreeProtected {
                path: path.to_owned(),
            }),
            Some(_) => {
                self.worktrees.remove(path);
                self.head_logs.remove(path);
                Ok(true)
            }
        };
        core::future::ready(result)
    }

    fn set_head(
        &mut self,
        path: &str,
        head: &Head,
        note: ReflogNote<'_>,
    ) -> impl Future<Output = Result<(), MemError>> + Send {
        let result = (|| {
            let old = self
                .worktrees
                .get(path)
                .map(|held| self.head_state(held))
                .ok_or_else(|| MemError::UnknownWorktree {
                    path: path.to_owned(),
                })?;
            match head {
                Head::Branch(new_branch) => {
                    self.check_not_checked_out(new_branch, Some(path))?;
                    let target = Ref::branch(new_branch.clone());
                    // Only a branch this worktree's HEAD names exists only
                    // as the head this move rewrites. Such an unborn branch
                    // may be excluded from the conflict scan; a born branch
                    // stays in `refs` and must be seen. A re-checkout of the
                    // branch HEAD is already on must also pass.
                    let own = match &old {
                        HeadState::Unborn { branch } => Some(Ref::branch(branch.clone())),
                        HeadState::Attached { branch, .. } if branch == new_branch => {
                            Some(Ref::branch(branch.clone()))
                        }
                        _ => None,
                    };
                    self.check_no_conflict(&target, own.as_ref())?;
                }
                Head::Detached(commit) => self.require_commit(*commit)?,
            }
            if let Some(worktree) = self.worktrees.get_mut(path) {
                worktree.head = head.clone();
            }
            let new = self
                .worktrees
                .get(path)
                .map(|held| self.head_state(held))
                .and_then(|state| state.commit());
            if let Some(new) = new {
                self.log_head(path, old.commit(), new, note);
            }
            Ok(())
        })();
        core::future::ready(result)
    }

    fn head(&self, path: &str) -> impl Future<Output = Result<Option<HeadState>, MemError>> + Send {
        let state = self.worktrees.get(path).map(|held| self.head_state(held));
        core::future::ready(Ok(state))
    }

    fn visit_ref_log<F>(
        &self,
        name: &RefName,
        visit: F,
    ) -> impl Future<Output = Result<(), MemError>> + Send
    where
        F: FnMut(&ReflogEntry) -> bool + Send,
    {
        visit_newest_first(self.ref_logs.get(name), visit);
        core::future::ready(Ok(()))
    }

    fn visit_head_log<F>(
        &self,
        path: &str,
        visit: F,
    ) -> impl Future<Output = Result<(), MemError>> + Send
    where
        F: FnMut(&ReflogEntry) -> bool + Send,
    {
        visit_newest_first(self.head_logs.get(path), visit);
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
