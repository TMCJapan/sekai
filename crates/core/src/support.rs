//! In-memory port fakes used by tests.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;
use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use crate::port::{BlobStore, MetaStore};
use sekai_util::{
    ApplyOutcome, BlobHash, ChunkCoord, ChunkHistoryEntry, Commit, CommitId, CommitTag, DiffHash,
    FoldOutcome, RegionFingerprint, RegionKey, RegionStateEntry, TagName,
};

/// Fake backend failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemError {
    /// The blob is absent from the fake store.
    MissingBlob,
    /// The tag name is taken.
    DuplicateTag,
    /// A named parent does not exist.
    UnknownParent {
        /// Parent that was asked for.
        id: u64,
    },
    /// A commit named itself as a parent.
    SelfParent {
        /// Commit that named itself.
        id: u64,
    },
    /// A retirement that does not follow the history.
    InvalidRetirement {
        /// Commit being retired.
        from: u64,
        /// Intended successor.
        into: u64,
    },
}

/// Parent links by commit, for the walks over the history graph.
fn parent_index(commits: &[Commit]) -> BTreeMap<CommitId, Vec<CommitId>> {
    commits
        .iter()
        .map(|commit| (commit.id, commit.parents.clone()))
        .collect()
}

/// Ancestors of `commit`: every commit reachable through parent
/// links, the commit itself excluded.
fn ancestor_ids(commits: &[Commit], commit: CommitId) -> BTreeSet<CommitId> {
    let index = parent_index(commits);
    let mut seen = BTreeSet::new();
    let mut stack: Vec<CommitId> = index.get(&commit).cloned().unwrap_or_default();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let Some(parents) = index.get(&id) {
            stack.extend(parents.iter().copied());
        }
    }
    seen
}

/// Effective rows at `commit`: exactly one row per coordinate known
/// then, tombstones included. Mirrors the backend: the row introduced at
/// the commit or at one of its ancestors, the nearest one winning.
fn effective_at(
    rows: &[ChunkHistoryEntry],
    commits: &[Commit],
    commit: CommitId,
) -> Vec<ChunkHistoryEntry> {
    let ancestors = ancestor_ids(commits, commit);
    let mut latest: BTreeMap<ChunkCoord, ChunkHistoryEntry> = BTreeMap::new();
    for row in rows {
        if row.commit != commit && !ancestors.contains(&row.commit) {
            continue;
        }
        latest
            .entry(row.coord)
            .and_modify(|kept| {
                if row.commit > kept.commit {
                    *kept = *row;
                }
            })
            .or_insert(*row);
    }
    latest.into_values().collect()
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
    /// Commits in ID order.
    pub commits: Vec<Commit>,
    /// Every recorded row.
    pub rows: Vec<ChunkHistoryEntry>,
    /// Derived region states.
    pub states: Vec<RegionStateEntry>,
    /// Tags by name.
    pub tags: BTreeMap<TagName, CommitTag>,
    /// The next identifier to hand out. Counted separately from the
    /// stored commits so a retirement never makes an identifier
    /// reusable.
    next_id: u64,
}

impl MetaStore for MemMeta {
    type Error = MemError;

    fn create_commit(
        &mut self,
        created_at_ms: u64,
        parents: &[CommitId],
    ) -> impl Future<Output = Result<CommitId, MemError>> + Send {
        let id = CommitId(self.next_id + 1);
        // Mirrors the backend: a parent must exist, a commit may not
        // name itself, and a parent listed twice is recorded once, in the
        // canonical (ascending identifier) order.
        let known: BTreeSet<CommitId> = self.commits.iter().map(|s| s.id).collect();
        for parent in parents {
            if *parent == id {
                return core::future::ready(Err(MemError::SelfParent { id: parent.0 }));
            }
            if !known.contains(parent) {
                return core::future::ready(Err(MemError::UnknownParent { id: parent.0 }));
            }
        }
        let mut unique: BTreeSet<CommitId> = BTreeSet::new();
        for parent in parents {
            unique.insert(*parent);
        }
        self.next_id = id.0;
        self.commits
            .push(Commit::new(id, created_at_ms, unique.into_iter().collect()));
        core::future::ready(Ok(id))
    }

    fn commit_parents(
        &self,
        commit: CommitId,
    ) -> impl Future<Output = Result<Vec<CommitId>, MemError>> + Send {
        let parents = self
            .commits
            .iter()
            .find(|s| s.id == commit)
            .map(|s| s.parents.clone())
            .unwrap_or_default();
        core::future::ready(Ok(parents))
    }

    fn record_chunk(
        &mut self,
        commit: CommitId,
        coord: &ChunkCoord,
        blob: Option<&BlobHash>,
        diff: Option<&DiffHash>,
    ) -> impl Future<Output = Result<(), MemError>> + Send {
        self.rows.push(ChunkHistoryEntry::new(
            *coord,
            commit,
            blob.copied(),
            diff.copied(),
        ));
        core::future::ready(Ok(()))
    }

    fn lookup_chunk(
        &self,
        commit: CommitId,
        coord: &ChunkCoord,
    ) -> impl Future<Output = Result<Option<ChunkHistoryEntry>, MemError>> + Send {
        // The row introduced at the commit or at one of its ancestors,
        // the nearest one winning (delta storage: unchanged chunks have
        // no row at newer commits).
        let ancestors = ancestor_ids(&self.commits, commit);
        core::future::ready(Ok(self
            .rows
            .iter()
            .filter(|row| {
                (row.commit == commit || ancestors.contains(&row.commit)) && row.coord == *coord
            })
            .max_by_key(|row| row.commit)
            .copied()))
    }

    fn lookup_commit(
        &self,
        id: CommitId,
    ) -> impl Future<Output = Result<Option<Commit>, MemError>> + Send {
        core::future::ready(Ok(self.commits.iter().find(|s| s.id == id).cloned()))
    }

    fn latest_commit(&self) -> impl Future<Output = Result<Option<Commit>, MemError>> + Send {
        core::future::ready(Ok(self.commits.last().cloned()))
    }

    fn visit_commit_chunks<F>(
        &self,
        commit: CommitId,
        mut visit: F,
    ) -> impl Future<Output = Result<(), MemError>> + Send
    where
        F: FnMut(&ChunkHistoryEntry) -> bool + Send,
    {
        for row in effective_at(&self.rows, &self.commits, commit) {
            if !visit(&row) {
                break;
            }
        }
        core::future::ready(Ok(()))
    }

    fn visit_commits<F>(&self, mut visit: F) -> impl Future<Output = Result<(), MemError>> + Send
    where
        F: FnMut(&Commit) -> bool + Send,
    {
        for commit in &self.commits {
            if !visit(commit) {
                break;
            }
        }
        core::future::ready(Ok(()))
    }

    fn visit_fresh_rows<F>(
        &self,
        commit: CommitId,
        mut visit: F,
    ) -> impl Future<Output = Result<(), MemError>> + Send
    where
        F: FnMut(&ChunkHistoryEntry) -> bool + Send,
    {
        for row in self.rows.iter().filter(|row| row.commit == commit) {
            if !visit(row) {
                break;
            }
        }
        core::future::ready(Ok(()))
    }

    fn load_region_states(
        &self,
    ) -> impl Future<Output = Result<Vec<RegionStateEntry>, MemError>> + Send {
        core::future::ready(Ok(self.states.clone()))
    }

    async fn apply_commit_incremental(
        &mut self,
        created_at_ms: u64,
        parents: &[CommitId],
        entries: &[sekai_util::CommitEntry],
        carry_from: Option<(CommitId, &[RegionKey])>,
        fingerprints: &[RegionFingerprint],
        removed: &[RegionKey],
    ) -> Result<ApplyOutcome, MemError> {
        let id = self.create_commit(created_at_ms, parents).await?;
        for entry in entries {
            self.rows.push(ChunkHistoryEntry::new(
                entry.coord,
                id,
                entry.blob,
                entry.diff,
            ));
        }
        // Delta storage: carried regions contribute no rows. Their
        // effective chunks are counted from the previous commit for the
        // report; only derived state advances.
        let mut carried_chunks = 0usize;
        if let Some((prev, keys)) = carry_from {
            for row in effective_at(&self.rows, &self.commits, prev) {
                if row.blob.is_some() && keys.contains(&RegionKey::of(row.coord)) {
                    carried_chunks += 1;
                }
            }
            for key in keys {
                for state in &mut self.states {
                    if state.fingerprint.key == *key {
                        state.commit_id = id;
                    }
                }
            }
        }
        for fp in fingerprints {
            let entry = RegionStateEntry {
                fingerprint: *fp,
                commit_id: id,
            };
            if let Some(state) = self.states.iter_mut().find(|s| s.fingerprint.key == fp.key) {
                *state = entry;
            } else {
                self.states.push(entry);
            }
        }
        self.states
            .retain(|state| !removed.contains(&state.fingerprint.key));
        Ok(ApplyOutcome { id, carried_chunks })
    }

    fn tag_commit(
        &mut self,
        name: &TagName,
        commit: CommitId,
        created_at_ms: u64,
    ) -> impl Future<Output = Result<(), MemError>> + Send {
        // Mirrors the backend PRIMARY KEY: duplicates are rejected.
        if self.tags.contains_key(name) {
            return core::future::ready(Err(MemError::DuplicateTag));
        }
        self.tags.insert(
            name.clone(),
            CommitTag::new(name.clone(), commit, created_at_ms),
        );
        core::future::ready(Ok(()))
    }

    fn untag(&mut self, name: &TagName) -> impl Future<Output = Result<bool, MemError>> + Send {
        core::future::ready(Ok(self.tags.remove(name).is_some()))
    }

    fn lookup_tag(
        &self,
        name: &TagName,
    ) -> impl Future<Output = Result<Option<CommitTag>, MemError>> + Send {
        core::future::ready(Ok(self.tags.get(name).cloned()))
    }

    fn visit_tags<F>(&self, mut visit: F) -> impl Future<Output = Result<(), MemError>> + Send
    where
        F: FnMut(&CommitTag) -> bool + Send,
    {
        // BTreeMap iteration is name order, matching the backend query.
        for tag in self.tags.values() {
            if !visit(tag) {
                break;
            }
        }
        core::future::ready(Ok(()))
    }

    fn retire_commit(
        &mut self,
        from: CommitId,
        into: CommitId,
    ) -> impl Future<Output = Result<FoldOutcome, MemError>> + Send {
        // Mirrors the backend: a retirement folds into a strictly newer,
        // existing commit that descends from the retired one. Folding
        // into an unrelated history would graft rows the successor never
        // had.
        if into <= from || !self.commits.iter().any(|s| s.id == into) {
            return core::future::ready(Err(MemError::InvalidRetirement {
                from: from.0,
                into: into.0,
            }));
        }
        if !ancestor_ids(&self.commits, into).contains(&from) {
            return core::future::ready(Err(MemError::InvalidRetirement {
                from: from.0,
                into: into.0,
            }));
        }
        // Rows superseded by the history `into` continues are dropped;
        // the survivors move onto it.
        let mut superseding = ancestor_ids(&self.commits, into);
        superseding.insert(into);
        let mut folded = 0usize;
        let mut dropped = 0usize;
        let mut kept: Vec<ChunkHistoryEntry> = Vec::new();
        for row in &self.rows {
            if row.commit != from {
                kept.push(*row);
                continue;
            }
            let superseded = self.rows.iter().any(|other| {
                other.coord == row.coord
                    && other.commit > from
                    && superseding.contains(&other.commit)
            });
            if superseded {
                dropped += 1;
            } else {
                let mut moved = *row;
                moved.commit = into;
                kept.push(moved);
                folded += 1;
            }
        }
        self.rows = kept;
        for state in &mut self.states {
            if state.commit_id == from {
                state.commit_id = into;
            }
        }
        // The retired commit leaves the graph: its children continue the
        // history of its own parents.
        let from_parents = self
            .commits
            .iter()
            .find(|s| s.id == from)
            .map(|s| s.parents.clone())
            .unwrap_or_default();
        for commit in &mut self.commits {
            if !commit.parents.contains(&from) {
                continue;
            }
            // The backend drops every edge naming the retired commit.
            commit.parents.retain(|parent| *parent != from);
            for parent in &from_parents {
                if !commit.parents.contains(parent) {
                    commit.parents.push(*parent);
                }
            }
            commit.parents.sort();
        }
        self.commits.retain(|commit| commit.id != from);
        self.tags.retain(|_, tag| tag.commit != from);
        core::future::ready(Ok(FoldOutcome { folded, dropped }))
    }
}

/// In-memory [`BlobStore`].
#[derive(Debug, Default)]
pub struct MemCas {
    blobs: BTreeMap<BlobHash, Vec<u8>>,
}

impl BlobStore for MemCas {
    type Error = MemError;

    fn contains(&self, hash: &BlobHash) -> impl Future<Output = Result<bool, Self::Error>> + Send {
        core::future::ready(Ok(self.blobs.contains_key(hash)))
    }

    fn put(
        &mut self,
        hash: &BlobHash,
        payload: &[u8],
    ) -> impl Future<Output = Result<bool, Self::Error>> + Send {
        if self.blobs.contains_key(hash) {
            return core::future::ready(Ok(false));
        }
        self.blobs.insert(*hash, payload.to_vec());
        core::future::ready(Ok(true))
    }

    fn sync(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        core::future::ready(Ok(()))
    }

    fn fetch_into(
        &self,
        hash: &BlobHash,
        out: &mut Vec<u8>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        out.clear();
        let Some(bytes) = self.blobs.get(hash) else {
            return core::future::ready(Err(MemError::MissingBlob));
        };
        out.extend_from_slice(bytes);
        core::future::ready(Ok(()))
    }

    fn remove(
        &mut self,
        hash: &BlobHash,
    ) -> impl Future<Output = Result<bool, Self::Error>> + Send {
        core::future::ready(Ok(self.blobs.remove(hash).is_some()))
    }

    fn visit_blobs<F>(&self, mut visit: F) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        F: FnMut(&BlobHash) -> bool + Send,
    {
        for hash in self.blobs.keys() {
            if !visit(hash) {
                break;
            }
        }
        core::future::ready(Ok(()))
    }
}
