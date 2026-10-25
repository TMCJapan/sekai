//! In-memory port fakes used by tests.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;
use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use crate::port::{BlobStore, MetaStore};
use sekai_util::{
    ApplyOutcome, BlobHash, ChunkCoord, ChunkHistoryEntry, Commit, CommitId, CommitRef, DiffHash,
    FoldOutcome, RefName, RegionFingerprint, RegionKey, RegionStateEntry,
};

/// Fake backend failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemError {
    /// The blob is absent from the fake store.
    MissingBlob,
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
    /// A retirement that does not follow the history.
    InvalidRetirement {
        /// Commit being retired.
        from: CommitId,
        /// Intended successor.
        into: CommitId,
    },
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
///
/// "Nearest" follows the local sequence, never the commit hash: hashes
/// carry no timeline order.
fn effective_at(
    rows: &[ChunkHistoryEntry],
    commits: &[Commit],
    seqs: &BTreeMap<CommitId, u64>,
    commit: CommitId,
) -> Vec<ChunkHistoryEntry> {
    let ancestors = ancestor_ids(commits, commit);
    let Some(target) = seqs.get(&commit).copied() else {
        return Vec::new();
    };
    let mut latest: BTreeMap<ChunkCoord, (u64, ChunkHistoryEntry)> = BTreeMap::new();
    for row in rows {
        if row.commit != commit && !ancestors.contains(&row.commit) {
            continue;
        }
        // A row whose commit has no local sequence is corruption:
        // retirement re-stamps or drops rows before forgetting a seq.
        let Some(seq) = seqs.get(&row.commit).copied() else {
            continue;
        };
        if seq > target {
            continue;
        }
        latest
            .entry(row.coord)
            .and_modify(|(kept_seq, kept)| {
                if seq > *kept_seq {
                    *kept_seq = seq;
                    *kept = *row;
                }
            })
            .or_insert((seq, *row));
    }
    latest.into_values().map(|(_, row)| row).collect()
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
    /// Every recorded row.
    pub rows: Vec<ChunkHistoryEntry>,
    /// Derived region states.
    pub states: Vec<RegionStateEntry>,
    /// Refs by name.
    pub refs: BTreeMap<RefName, CommitRef>,
    /// Stable local sequence per commit hash. Retirement never renumbers
    /// surviving commits.
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
    fn insert_commit(&mut self, id: CommitId, created_at_ms: u64, parents: Vec<CommitId>) -> bool {
        if self.seqs.contains_key(&id) {
            return false;
        }
        self.next_seq += 1;
        self.seqs.insert(id, self.next_seq);
        self.commits.push(Commit::new(id, created_at_ms, parents));
        true
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
        let id = crate::commit::commit_hash(&canonical, created_at_ms, &[], &[]);
        self.insert_commit(id, created_at_ms, canonical);
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
            .map(|c| c.parents.clone())
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
        // no row at newer commits). Ordering uses the local sequence,
        // never the commit hash.
        let ancestors = ancestor_ids(&self.commits, commit);
        let target = self.seq_of(commit);
        let found = target.and_then(|target| {
            self.rows
                .iter()
                .filter(|row| {
                    (row.commit == commit || ancestors.contains(&row.commit)) && row.coord == *coord
                })
                .filter_map(|row| {
                    self.seq_of(row.commit)
                        .filter(|seq| *seq <= target)
                        .map(|seq| (seq, row))
                })
                .max_by_key(|(seq, _)| *seq)
                .map(|(_, row)| *row)
        });
        core::future::ready(Ok(found))
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

    fn visit_commit_chunks<F>(
        &self,
        commit: CommitId,
        mut visit: F,
    ) -> impl Future<Output = Result<(), MemError>> + Send
    where
        F: FnMut(&ChunkHistoryEntry) -> bool + Send,
    {
        for row in effective_at(&self.rows, &self.commits, &self.seqs, commit) {
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
        let mut ordered: Vec<&Commit> = self.commits.iter().collect();
        ordered.sort_by_key(|commit| self.seq_of(commit.id));
        for commit in ordered {
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
        within: Option<CommitId>,
    ) -> impl Future<Output = Result<Vec<RegionStateEntry>, MemError>> + Send {
        // Only fingerprints confirmed inside the given history may be
        // carried: one confirmed elsewhere describes another state of the
        // file. An unborn history (`None`) knows no file at all.
        let ancestors = within.map_or_else(BTreeSet::new, |commit| {
            let mut set = ancestor_ids(&self.commits, commit);
            set.insert(commit);
            set
        });
        let states = self
            .states
            .iter()
            .filter(|state| ancestors.contains(&state.commit_id))
            .copied()
            .collect();
        core::future::ready(Ok(states))
    }

    fn apply_commit_incremental(
        &mut self,
        created_at_ms: u64,
        parents: &[CommitId],
        entries: &[sekai_util::CommitEntry],
        carry_from: Option<(CommitId, &[RegionKey])>,
        fingerprints: &[RegionFingerprint],
        removed: &[RegionKey],
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
        // The last `?` step: it runs before any row or state write, so a
        // rejection leaves the store unchanged. Do not add fallible work
        // after this point until the ref record is re-inserted.
        //
        // Panics (including allocation failure) are outside this fake's
        // model: unlike SQLite's transaction, the in-memory writes cannot
        // roll back.
        let mut unique: BTreeSet<CommitId> = BTreeSet::new();
        for parent in parents {
            if !self.commits.iter().any(|commit| commit.id == *parent) {
                return core::future::ready(Err(MemError::UnknownParent { id: *parent }));
            }
            unique.insert(*parent);
        }
        let canonical: Vec<CommitId> = unique.into_iter().collect();
        let id = crate::commit::commit_hash(&canonical, created_at_ms, entries, removed);
        if self.insert_commit(id, created_at_ms, canonical) {
            for entry in entries {
                self.rows.push(ChunkHistoryEntry::new(
                    entry.coord,
                    id,
                    entry.blob,
                    entry.diff,
                ));
            }
        }
        // Delta storage: carried regions contribute no rows. Their
        // effective chunks are counted from the previous commit for the
        // report; only derived state advances.
        let mut carried_chunks = 0usize;
        if let Some((prev, keys)) = carry_from {
            for row in effective_at(&self.rows, &self.commits, &self.seqs, prev) {
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
        // The active ref moves onto the new commit, in the same step: a
        // ref that lags behind its history would fork the next commit.
        // No fallible step follows the write phase; the bound record is
        // re-inserted here, so the active ref cannot lag.
        record.commit = Some(id);
        self.refs.insert(active, record);
        core::future::ready(Ok(ApplyOutcome { id, carried_chunks }))
    }

    fn retire_commit(
        &mut self,
        from: CommitId,
        into: CommitId,
    ) -> impl Future<Output = Result<FoldOutcome, MemError>> + Send {
        // Mirrors the backend: a retirement folds into a strictly newer,
        // existing commit that descends from the retired one. Folding
        // into an unrelated history would graft rows the successor never
        // had. "Newer" follows the local sequence, not the hash.
        let (Some(from_seq), Some(into_seq)) = (self.seq_of(from), self.seq_of(into)) else {
            return core::future::ready(Err(MemError::InvalidRetirement { from, into }));
        };
        if into_seq <= from_seq || !ancestor_ids(&self.commits, into).contains(&from) {
            return core::future::ready(Err(MemError::InvalidRetirement { from, into }));
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
                    && self.seq_of(other.commit).is_some_and(|seq| seq > from_seq)
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
            .find(|c| c.id == from)
            .map(|c| c.parents.clone())
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
        // The ID may be recorded again later; forget its sequence so the
        // re-added commit gets a fresh position at the end of the timeline.
        self.seqs.remove(&from);
        // A ref naming the retired commit follows the fold: the
        // successor holds the same effective state, so the ref keeps
        // naming it.
        for record in self.refs.values_mut() {
            if record.commit == Some(from) {
                record.commit = Some(into);
            }
        }
        core::future::ready(Ok(FoldOutcome { folded, dropped }))
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
        // Moving a ref keeps its creation time; only a new ref records one.
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
        // BTreeMap iteration is name order, matching the backend query.
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
        // Mirrors the backend: the target commit must exist. A dangling
        // target is corruption, not an unborn ref.
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
