//! Incremental world backup over abstract ports.
//!
//! Rationale: backup never parses NBT - the CAS key runs over raw sector
//! bytes, so the hot path is read + hash + store with zero decoding.
//! Change detection (`diff`) is deliberately not computed here; parsing
//! every chunk would multiply scan cost for data no consumer reads yet.
//!
//! Unchanged region files skip ingestion entirely: each file carries a
//! `(mtime, size, header hash)` fingerprint in derived state, and a file
//! matching all three signals keeps its previous history rows via the
//! port's carry seam instead of a per-chunk loop. Tombstones keep history
//! total without a global chunk census: the known universe is exactly the
//! coordinate set of the latest snapshot, so every snapshot re-records
//! every known coordinate (present, carried, or tombstone).
//!
//! The orchestration is split so concrete adapters (threading, filesystem,
//! clocks, timing) stay outside `core`:
//!
//! ```text
//! plan_backup   read previous state, decide carry vs ingest (pure policy)
//!   -> adapter ingests changed regions (hash + CAS put per chunk)
//! assemble      merge ingested rows, carried presence, and tombstones (pure)
//!   -> adapter flushes CAS (durability barrier before metadata)
//! commit        record the snapshot atomically (single metadata batch)
//! ```
//!
//! Crash order: every blob is flushed to CAS *before* the single metadata
//! batch commits, so a torn backup leaves at most orphan blobs (reclaimed
//! by future GC), never dangling references.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use crate::domain::coords::ChunkCoord;
use crate::domain::hash::BlobHash;
use crate::domain::region::{
    ApplyOutcome, RegionFingerprint, RegionKey, RegionStateEntry, SnapshotEntry,
};
use crate::domain::snapshot::{Snapshot, SnapshotId};
use crate::port::hash::BlobHasher;
use crate::port::meta::MetaStore;

/// Outcome of one backup run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupReport {
    /// Newly recorded snapshot ID.
    pub snapshot: SnapshotId,
    /// Chunks present on disk in this backup.
    pub chunks: usize,
    /// Blobs newly written to CAS (the rest deduplicated).
    pub new_blobs: usize,
    /// Tombstone rows recorded for vanished chunks.
    pub tombstones: usize,
    /// Region files skipped via fingerprint match.
    pub skipped_regions: usize,
    /// History rows carried over for skipped regions.
    pub carried_chunks: usize,
}

/// Previous snapshot plus the state the walk needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Previous {
    /// Latest snapshot, when the store is non-empty.
    pub snapshot: Option<Snapshot>,
    /// Coordinates of the latest snapshot (empty on first run).
    pub universe: BTreeSet<ChunkCoord>,
    /// Stored fingerprints keyed by region identity.
    pub states: BTreeMap<RegionKey, RegionStateEntry>,
}

/// One region file observed on disk by the adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observation {
    /// Which region file was observed.
    pub key: RegionKey,
    /// Fresh fingerprint of the file.
    pub fingerprint: RegionFingerprint,
}

/// Staged backup decision: what to ingest, carry, or drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Fingerprint-matched regions, carried from the previous snapshot.
    pub carries: Vec<RegionKey>,
    /// Changed regions the adapter must ingest.
    pub ingest: Vec<RegionKey>,
    /// Stored regions gone from disk (state rows leave with this snapshot).
    pub removed: Vec<RegionKey>,
    /// Discovered region identities (for dropping stale state).
    pub discovered: BTreeSet<RegionKey>,
}

/// Merged rows ready to commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assembled {
    /// Fresh history rows (ingested chunks plus tombstones).
    pub entries: Vec<SnapshotEntry>,
    /// Coordinates present on disk (ingested plus carried).
    pub chunks: usize,
    /// Blobs newly written to CAS.
    pub new_blobs: usize,
    /// Tombstone rows recorded for vanished chunks.
    pub tombstones: usize,
    /// Fresh fingerprints for ingested files.
    pub fingerprints: Vec<RegionFingerprint>,
    /// Fingerprint-matched regions, carried from the previous snapshot.
    pub carries: Vec<RegionKey>,
    /// Stored regions gone from disk.
    pub removed: Vec<RegionKey>,
}

/// Load the previous snapshot's coordinates and region fingerprints, then
/// decide per observed region whether its rows carry over or it needs
/// ingestion.
///
/// Fingerprint and ingest open the file separately, and a carried file is
/// not re-read before the commit: a concurrent rewrite after the
/// fingerprint match is silently missed by this snapshot (the next run
/// mismatches and re-ingests). Callers must quiesce the server before
/// snapshotting. Unchanged files skip read/hash/CAS; their rows carry over
/// at commit time.
pub fn plan_backup<M: MetaStore>(
    meta: &M,
    observed: &[Observation],
) -> Result<(Previous, Plan), M::Error> {
    let mut universe: BTreeSet<ChunkCoord> = BTreeSet::new();
    let snapshot = meta.latest_snapshot()?;
    if let Some(latest) = snapshot {
        meta.visit_snapshot_chunks(latest.id, |entry| {
            universe.insert(entry.coord);
            true
        })?;
    }
    let mut states: BTreeMap<RegionKey, RegionStateEntry> = BTreeMap::new();
    for state in meta.load_region_states()? {
        states.insert(state.key, state);
    }
    let previous = Previous {
        snapshot,
        universe,
        states,
    };
    let mut plan = Plan {
        carries: Vec::new(),
        ingest: Vec::new(),
        removed: Vec::new(),
        discovered: BTreeSet::new(),
    };
    for obs in observed {
        plan.discovered.insert(obs.key);
        if previous
            .states
            .get(&obs.key)
            .is_some_and(|state| obs.fingerprint.matches_state(state))
        {
            plan.carries.push(obs.key);
        } else {
            plan.ingest.push(obs.key);
        }
    }
    for state in previous.states.values() {
        if !plan.discovered.contains(&state.key) {
            plan.removed.push(state.key);
        }
    }
    Ok((previous, plan))
}

/// Hash one raw chunk payload into its persistent CAS key.
///
/// The payload is hashed exactly as read from the `.mca` sector, including
/// its compression framing.
#[must_use]
pub fn hash_payload<H: BlobHasher>(payload: &[u8]) -> BlobHash {
    let mut hasher = H::new();
    hasher.update(payload);
    hasher.finalize()
}

/// Stage the history row for one ingested chunk.
///
/// The volatile `diff` view is deliberately not computed: parsing every
/// chunk would multiply scan cost for a cache no consumer reads yet.
#[must_use]
pub const fn stage_present(coord: ChunkCoord, hash: BlobHash) -> SnapshotEntry {
    SnapshotEntry::new(coord, Some(hash), None)
}

/// Merge ingested rows with carried presence and tombstones.
///
/// Coordinates under carried regions were fingerprint-matched, so their
/// rows carry over verbatim; every previously known coordinate that is
/// neither ingested nor carried becomes a tombstone. Consumes the plan:
/// assembly is the plan's single use.
#[must_use]
pub fn assemble(
    plan: Plan,
    previous: &Previous,
    ingested_entries: Vec<SnapshotEntry>,
    mut present: BTreeSet<ChunkCoord>,
    new_blobs: usize,
    fingerprints: Vec<RegionFingerprint>,
) -> Assembled {
    if !plan.carries.is_empty() {
        let skipped: BTreeSet<RegionKey> = plan.carries.iter().copied().collect();
        for coord in &previous.universe {
            if skipped.contains(&region_key_of(*coord)) {
                present.insert(*coord);
            }
        }
    }
    let mut entries = ingested_entries;
    let mut tombstones = 0usize;
    for coord in &previous.universe {
        if !present.contains(coord) {
            entries.push(SnapshotEntry::new(*coord, None, None));
            tombstones += 1;
        }
    }
    Assembled {
        chunks: present.len(),
        entries,
        new_blobs,
        tombstones,
        fingerprints,
        carries: plan.carries,
        removed: plan.removed,
    }
}

/// Record the assembled rows as one snapshot.
///
/// The caller must have flushed CAS (the port's `sync` barrier) before
/// invoking this: file data is fsynced per blob, and the barrier is what
/// makes every referenced blob crash-durable ahead of this single metadata
/// batch.
pub fn commit<M: MetaStore>(
    meta: &mut M,
    previous: &Previous,
    staged: &Assembled,
    created_at_ms: u64,
) -> Result<BackupReport, M::Error> {
    let carry_from = previous
        .snapshot
        .map(|snapshot| (snapshot.id, staged.carries.as_slice()));
    let outcome: ApplyOutcome = meta.apply_snapshot_incremental(
        created_at_ms,
        &staged.entries,
        carry_from,
        &staged.fingerprints,
        &staged.removed,
    )?;
    Ok(BackupReport {
        snapshot: outcome.id,
        chunks: staged.chunks,
        new_blobs: staged.new_blobs,
        tombstones: staged.tombstones,
        skipped_regions: staged.carries.len(),
        carried_chunks: outcome.carried_chunks,
    })
}

/// Region identity owning a chunk coordinate.
pub(crate) const fn region_key_of(coord: ChunkCoord) -> RegionKey {
    RegionKey::new(coord.dim, coord.kind, coord.region_x(), coord.region_z())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::coords::{Dimension, RegionKind};
    use crate::domain::history::ChunkHistoryEntry;

    /// Minimal in-memory [`MetaStore`] behind the backup seam.
    #[derive(Debug, Default)]
    struct MemMeta {
        snapshots: Vec<Snapshot>,
        rows: Vec<ChunkHistoryEntry>,
        states: Vec<RegionStateEntry>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct MemError;

    impl MetaStore for MemMeta {
        type Error = MemError;

        fn create_snapshot(&mut self, created_at_ms: u64) -> Result<SnapshotId, MemError> {
            let id = SnapshotId(self.snapshots.len() as u64 + 1);
            self.snapshots.push(Snapshot::new(id, created_at_ms));
            Ok(id)
        }

        fn record_chunk(
            &mut self,
            snapshot: SnapshotId,
            coord: &ChunkCoord,
            blob: Option<&BlobHash>,
            diff: Option<&crate::domain::hash::DiffHash>,
        ) -> Result<(), MemError> {
            self.rows.push(ChunkHistoryEntry::new(
                *coord,
                snapshot,
                blob.copied(),
                diff.copied(),
            ));
            Ok(())
        }

        fn lookup_chunk(
            &self,
            snapshot: SnapshotId,
            coord: &ChunkCoord,
        ) -> Result<Option<ChunkHistoryEntry>, MemError> {
            Ok(self
                .rows
                .iter()
                .find(|row| row.snapshot == snapshot && row.coord == *coord)
                .copied())
        }

        fn lookup_snapshot(&self, id: SnapshotId) -> Result<Option<Snapshot>, MemError> {
            Ok(self.snapshots.iter().find(|s| s.id == id).copied())
        }

        fn latest_snapshot(&self) -> Result<Option<Snapshot>, MemError> {
            Ok(self.snapshots.last().copied())
        }

        fn visit_snapshot_chunks<F>(
            &self,
            snapshot: SnapshotId,
            mut visit: F,
        ) -> Result<(), MemError>
        where
            F: FnMut(&ChunkHistoryEntry) -> bool,
        {
            for row in &self.rows {
                if row.snapshot == snapshot && !visit(row) {
                    break;
                }
            }
            Ok(())
        }

        fn visit_snapshots<F>(&self, mut visit: F) -> Result<(), MemError>
        where
            F: FnMut(&Snapshot) -> bool,
        {
            for snapshot in &self.snapshots {
                if !visit(snapshot) {
                    break;
                }
            }
            Ok(())
        }

        fn load_region_states(&self) -> Result<Vec<RegionStateEntry>, MemError> {
            Ok(self.states.clone())
        }

        fn apply_snapshot_incremental(
            &mut self,
            created_at_ms: u64,
            entries: &[SnapshotEntry],
            carry_from: Option<(SnapshotId, &[RegionKey])>,
            fingerprints: &[RegionFingerprint],
            removed: &[RegionKey],
        ) -> Result<ApplyOutcome, MemError> {
            let id = self.create_snapshot(created_at_ms)?;
            for entry in entries {
                self.rows.push(ChunkHistoryEntry::new(
                    entry.coord,
                    id,
                    entry.blob,
                    entry.diff,
                ));
            }
            let mut carried_chunks = 0usize;
            if let Some((prev, keys)) = carry_from {
                let owned: Vec<ChunkHistoryEntry> = self
                    .rows
                    .iter()
                    .filter(|row| row.snapshot == prev && keys.contains(&region_key_of(row.coord)))
                    .map(|row| ChunkHistoryEntry::new(row.coord, id, row.blob, row.diff))
                    .collect();
                carried_chunks = owned.len();
                self.rows.extend(owned);
                for key in keys {
                    for state in &mut self.states {
                        if state.key == *key {
                            state.snapshot_id = id;
                        }
                    }
                }
            }
            for fp in fingerprints {
                if let Some(state) = self.states.iter_mut().find(|s| s.key == fp.key) {
                    *state = RegionStateEntry {
                        key: fp.key,
                        mtime_ms: fp.mtime_ms,
                        size: fp.size,
                        header_hash: fp.header_hash,
                        snapshot_id: id,
                    };
                } else {
                    self.states.push(RegionStateEntry {
                        key: fp.key,
                        mtime_ms: fp.mtime_ms,
                        size: fp.size,
                        header_hash: fp.header_hash,
                        snapshot_id: id,
                    });
                }
            }
            self.states.retain(|state| !removed.contains(&state.key));
            Ok(ApplyOutcome { id, carried_chunks })
        }
    }

    const OVER: Dimension = Dimension::OVERWORLD;
    const REGION: RegionKind = RegionKind::REGION;

    const fn key(rx: i32, rz: i32) -> RegionKey {
        RegionKey::new(OVER, REGION, rx, rz)
    }

    const fn coord(x: i32, z: i32) -> ChunkCoord {
        ChunkCoord::new(OVER, REGION, x, z)
    }

    fn fingerprint(key: RegionKey) -> RegionFingerprint {
        RegionFingerprint {
            key,
            mtime_ms: Some(1_700_000_000_000),
            size: 8192,
            header_hash: [7; 32],
        }
    }

    #[test]
    fn first_backup_ingests_everything() {
        let meta = MemMeta::default();
        let observed = [Observation {
            key: key(0, 0),
            fingerprint: fingerprint(key(0, 0)),
        }];
        let (previous, plan) = plan_backup(&meta, &observed).expect("plan must succeed");
        assert!(previous.snapshot.is_none());
        assert!(previous.universe.is_empty());
        assert!(plan.carries.is_empty());
        assert_eq!(plan.ingest, alloc::vec![key(0, 0)]);
        assert!(plan.removed.is_empty());
    }

    #[test]
    fn changed_region_reingests_while_rest_carries() {
        let mut meta = MemMeta::default();
        let fp0 = fingerprint(key(0, 0));
        let fp1 = fingerprint(key(1, 0));
        meta.apply_snapshot_incremental(
            1_000,
            &[
                stage_present(coord(0, 0), BlobHash([1; 32])),
                stage_present(coord(32, 0), BlobHash([2; 32])),
            ],
            None,
            &[fp0, fp1],
            &[],
        )
        .expect("seed must succeed");

        // Region (0, 0) changed (size signal differs): ingest. Region (1, 0)
        // matches: carry.
        let mut changed = fp0;
        changed.size += 1;
        let observed = [
            Observation {
                key: key(0, 0),
                fingerprint: changed,
            },
            Observation {
                key: key(1, 0),
                fingerprint: fp1,
            },
        ];
        let (previous, plan) = plan_backup(&meta, &observed).expect("plan must succeed");
        assert_eq!(plan.carries, alloc::vec![key(1, 0)]);
        assert_eq!(plan.ingest, alloc::vec![key(0, 0)]);
        assert!(plan.removed.is_empty());

        // Re-ingested chunk gets the new blob; the carried chunk keeps its
        // blob without re-ingest.
        let staged = assemble(
            plan,
            &previous,
            alloc::vec![stage_present(coord(0, 0), BlobHash([9; 32]))],
            BTreeSet::from([coord(0, 0)]),
            1,
            alloc::vec![changed],
        );
        assert_eq!(staged.chunks, 2);
        assert_eq!(staged.tombstones, 0);

        let report = commit(&mut meta, &previous, &staged, 2_000).expect("commit must succeed");
        assert_eq!(report.chunks, 2);
        assert_eq!(report.new_blobs, 1);
        assert_eq!(report.skipped_regions, 1);
        assert_eq!(report.carried_chunks, 1);
        assert_eq!(report.tombstones, 0);

        let got = meta
            .lookup_chunk(report.snapshot, &coord(0, 0))
            .expect("lookup must succeed")
            .expect("row must exist");
        assert_eq!(got.blob, Some(BlobHash([9; 32])));
        let kept = meta
            .lookup_chunk(report.snapshot, &coord(32, 0))
            .expect("lookup must succeed")
            .expect("row must exist");
        assert_eq!(kept.blob, Some(BlobHash([2; 32])));
    }

    #[test]
    fn missing_coordinates_become_tombstones_and_states_drop() {
        let mut meta = MemMeta::default();
        let s1 = meta
            .apply_snapshot_incremental(
                1_000,
                &[
                    stage_present(coord(0, 0), BlobHash([1; 32])),
                    stage_present(coord(1, 0), BlobHash([2; 32])),
                ],
                None,
                &[fingerprint(key(0, 0))],
                &[],
            )
            .expect("seed must succeed")
            .id;
        assert_eq!(s1, SnapshotId(1));

        // Region file gone from disk: removed state, tombstones for both.
        let (previous, plan) = plan_backup(&meta, &[]).expect("plan must succeed");
        assert_eq!(plan.removed, alloc::vec![key(0, 0)]);
        let staged = assemble(
            plan,
            &previous,
            alloc::vec::Vec::new(),
            BTreeSet::new(),
            0,
            alloc::vec::Vec::new(),
        );
        assert_eq!(staged.chunks, 0);
        assert_eq!(staged.tombstones, 2);

        let report = commit(&mut meta, &previous, &staged, 2_000).expect("commit must succeed");
        assert_eq!(report.tombstones, 2);
        assert!(meta.states.is_empty());
        let tomb = meta
            .lookup_chunk(report.snapshot, &coord(0, 0))
            .expect("lookup must succeed")
            .expect("row must exist");
        assert!(tomb.is_tombstone());
    }
}
