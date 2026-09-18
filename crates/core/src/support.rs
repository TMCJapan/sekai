//! In-memory port fakes used by tests.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use crate::port::{BlobStore, MetaStore};
use sekai_util::{
    ApplyOutcome, BlobHash, ChunkCoord, ChunkHistoryEntry, DiffHash, RegionFingerprint, RegionKey,
    RegionStateEntry, Snapshot, SnapshotId, SnapshotTag, TagName,
};

/// Fake backend failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemError;

/// Effective rows at `snapshot`: exactly one row per coordinate known then
/// (nearest row at or before it, tombstones included), ordered by
/// coordinate. Mirrors the backend GROUP BY query.
fn effective_at(rows: &[ChunkHistoryEntry], snapshot: SnapshotId) -> Vec<ChunkHistoryEntry> {
    let mut latest: BTreeMap<ChunkCoord, ChunkHistoryEntry> = BTreeMap::new();
    for row in rows {
        if row.snapshot <= snapshot {
            latest
                .entry(row.coord)
                .and_modify(|kept| {
                    if row.snapshot > kept.snapshot {
                        *kept = *row;
                    }
                })
                .or_insert(*row);
        }
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
    /// Snapshots in ID order.
    pub snapshots: Vec<Snapshot>,
    /// Every recorded row.
    pub rows: Vec<ChunkHistoryEntry>,
    /// Derived region states.
    pub states: Vec<RegionStateEntry>,
    /// Tags by name.
    pub tags: BTreeMap<TagName, SnapshotTag>,
}

impl MetaStore for MemMeta {
    type Error = MemError;

    fn create_snapshot(
        &mut self,
        created_at_ms: u64,
    ) -> impl Future<Output = Result<SnapshotId, MemError>> + Send {
        let id = SnapshotId(self.snapshots.len() as u64 + 1);
        self.snapshots.push(Snapshot::new(id, created_at_ms));
        core::future::ready(Ok(id))
    }

    fn record_chunk(
        &mut self,
        snapshot: SnapshotId,
        coord: &ChunkCoord,
        blob: Option<&BlobHash>,
        diff: Option<&DiffHash>,
    ) -> impl Future<Output = Result<(), MemError>> + Send {
        self.rows.push(ChunkHistoryEntry::new(
            *coord,
            snapshot,
            blob.copied(),
            diff.copied(),
        ));
        core::future::ready(Ok(()))
    }

    fn lookup_chunk(
        &self,
        snapshot: SnapshotId,
        coord: &ChunkCoord,
    ) -> impl Future<Output = Result<Option<ChunkHistoryEntry>, MemError>> + Send {
        // Nearest row at or before `snapshot` (delta storage: unchanged
        // chunks have no row at newer snapshots).
        core::future::ready(Ok(self
            .rows
            .iter()
            .filter(|row| row.snapshot <= snapshot && row.coord == *coord)
            .max_by_key(|row| row.snapshot)
            .copied()))
    }

    fn lookup_snapshot(
        &self,
        id: SnapshotId,
    ) -> impl Future<Output = Result<Option<Snapshot>, MemError>> + Send {
        core::future::ready(Ok(self.snapshots.iter().find(|s| s.id == id).copied()))
    }

    fn latest_snapshot(&self) -> impl Future<Output = Result<Option<Snapshot>, MemError>> + Send {
        core::future::ready(Ok(self.snapshots.last().copied()))
    }

    fn visit_snapshot_chunks<F>(
        &self,
        snapshot: SnapshotId,
        mut visit: F,
    ) -> impl Future<Output = Result<(), MemError>> + Send
    where
        F: FnMut(&ChunkHistoryEntry) -> bool + Send,
    {
        for row in effective_at(&self.rows, snapshot) {
            if !visit(&row) {
                break;
            }
        }
        core::future::ready(Ok(()))
    }

    fn visit_snapshots<F>(&self, mut visit: F) -> impl Future<Output = Result<(), MemError>> + Send
    where
        F: FnMut(&Snapshot) -> bool + Send,
    {
        for snapshot in &self.snapshots {
            if !visit(snapshot) {
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

    async fn apply_snapshot_incremental(
        &mut self,
        created_at_ms: u64,
        entries: &[sekai_util::SnapshotEntry],
        carry_from: Option<(SnapshotId, &[RegionKey])>,
        fingerprints: &[RegionFingerprint],
        removed: &[RegionKey],
    ) -> Result<ApplyOutcome, MemError> {
        let id = self.create_snapshot(created_at_ms).await?;
        for entry in entries {
            self.rows.push(ChunkHistoryEntry::new(
                entry.coord,
                id,
                entry.blob,
                entry.diff,
            ));
        }
        // Delta storage: carried regions contribute no rows. Their
        // effective chunks are counted from the previous snapshot for the
        // report; only derived state advances.
        let mut carried_chunks = 0usize;
        if let Some((prev, keys)) = carry_from {
            for row in effective_at(&self.rows, prev) {
                if row.blob.is_some() && keys.contains(&RegionKey::of(row.coord)) {
                    carried_chunks += 1;
                }
            }
            for key in keys {
                for state in &mut self.states {
                    if state.key == *key {
                        state.snapshot_id = id;
                    }
                }
            }
        }
        for fp in fingerprints {
            let entry = RegionStateEntry {
                key: fp.key,
                mtime_ms: fp.mtime_ms,
                size: fp.size,
                header_hash: fp.header_hash,
                snapshot_id: id,
            };
            if let Some(state) = self.states.iter_mut().find(|s| s.key == fp.key) {
                *state = entry;
            } else {
                self.states.push(entry);
            }
        }
        self.states.retain(|state| !removed.contains(&state.key));
        Ok(ApplyOutcome { id, carried_chunks })
    }

    fn tag_snapshot(
        &mut self,
        name: &TagName,
        snapshot: SnapshotId,
        created_at_ms: u64,
    ) -> impl Future<Output = Result<(), MemError>> + Send {
        // Mirrors the backend PRIMARY KEY: duplicates are rejected.
        if self.tags.contains_key(name) {
            return core::future::ready(Err(MemError));
        }
        self.tags.insert(
            name.clone(),
            SnapshotTag::new(name.clone(), snapshot, created_at_ms),
        );
        core::future::ready(Ok(()))
    }

    fn untag(&mut self, name: &TagName) -> impl Future<Output = Result<bool, MemError>> + Send {
        core::future::ready(Ok(self.tags.remove(name).is_some()))
    }

    fn lookup_tag(
        &self,
        name: &TagName,
    ) -> impl Future<Output = Result<Option<SnapshotTag>, MemError>> + Send {
        core::future::ready(Ok(self.tags.get(name).cloned()))
    }

    fn visit_tags<F>(&self, mut visit: F) -> impl Future<Output = Result<(), MemError>> + Send
    where
        F: FnMut(&SnapshotTag) -> bool + Send,
    {
        // BTreeMap iteration is name order, matching the backend query.
        for tag in self.tags.values() {
            if !visit(tag) {
                break;
            }
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
            return core::future::ready(Err(MemError));
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
