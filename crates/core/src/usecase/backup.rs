//! Incremental world backup over abstract ports.
//!
//! Change detection (`diff`) is deliberately not computed here; parsing
//! every chunk would multiply scan cost for data no consumer reads yet.
//!
//! Unchanged region files skip ingestion entirely: each file carries a
//! `(mtime, size, header hash)` fingerprint in derived state, and a file
//! matching all three signals contributes no new rows - its previous rows
//! stay readable through fallback instead of being copied. Tombstones keep history total without a global
//! chunk census: the known universe is exactly the effective coordinate set
//! of the latest snapshot, so every snapshot records fresh rows only for
//! ingested chunks plus tombstones for vanished coordinates.
//!
//! The orchestration is split so concrete adapters (parallelism,
//! filesystem, clocks, timing) stay outside `core`:
//!
//! ```text
//! plan_backup   read previous state, decide carry vs ingest (policy)
//!   -> adapter ingests changed regions (hash + CAS put per chunk)
//! assemble      merge ingested rows, carried presence, tombstones (pure)
//!   -> adapter flushes CAS (durability barrier before metadata)
//! commit        record the snapshot atomically (single metadata batch)
//! ```
//!
//! Crash order: every blob is flushed to CAS *before* the single metadata
//! batch commits, so a torn backup leaves at most orphan blobs (reclaimed
//! by future GC), never dangling references.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use crate::port::meta::MetaStore;
use sekai_util::{
    ApplyOutcome, BlobHash, ChunkCoord, RegionFingerprint, RegionKey, RegionStateEntry, Scope,
    Snapshot, SnapshotEntry, SnapshotId,
};

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

/// Load prior state and classify observed regions as carried or ingested.
///
/// Regions outside `scope` are ignored entirely: they are neither carried
/// nor ingested, and their stored state rows are never marked removed.
/// Callers must quiesce the source while fingerprinting; a concurrent rewrite
/// after the match is not detected until a later backup.
pub async fn plan_backup<M: MetaStore>(
    meta: &M,
    observed: &[Observation],
    scope: Scope<'_>,
) -> Result<(Previous, Plan), M::Error> {
    let mut universe: BTreeSet<ChunkCoord> = BTreeSet::new();
    let snapshot = meta.latest_snapshot().await?;
    if let Some(latest) = snapshot {
        meta.visit_snapshot_chunks(latest.id, |entry| {
            universe.insert(entry.coord);
            true
        })
        .await?;
    }
    let mut states: BTreeMap<RegionKey, RegionStateEntry> = BTreeMap::new();
    for state in meta.load_region_states().await? {
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
        if !scope.matches_region(obs.key) {
            continue;
        }
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
        if scope.matches_region(state.key) && !plan.discovered.contains(&state.key) {
            plan.removed.push(state.key);
        }
    }
    Ok((previous, plan))
}

/// Stage an ingested chunk without computing its volatile diff hash.
pub const fn stage_present(coord: ChunkCoord, hash: BlobHash) -> SnapshotEntry {
    SnapshotEntry::new(coord, Some(hash), None)
}

/// Merge fresh rows with carried coordinates and tombstones.
///
/// Previously known coordinates inside `scope` but not present in the new
/// scan become tombstones; out-of-scope coordinates keep resolving through
/// fallback, so scoped backups never record spurious tombstones.
pub fn assemble(
    plan: Plan,
    previous: &Previous,
    ingested_entries: Vec<SnapshotEntry>,
    mut present: BTreeSet<ChunkCoord>,
    new_blobs: usize,
    fingerprints: Vec<RegionFingerprint>,
    scope: Scope<'_>,
) -> Assembled {
    if !plan.carries.is_empty() {
        let skipped: BTreeSet<RegionKey> = plan.carries.iter().copied().collect();
        for coord in &previous.universe {
            if scope.contains(*coord) && skipped.contains(&RegionKey::of(*coord)) {
                present.insert(*coord);
            }
        }
    }
    let mut entries = ingested_entries;
    let mut tombstones = 0usize;
    for coord in &previous.universe {
        if scope.contains(*coord) && !present.contains(coord) {
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

/// Commit assembled rows as one snapshot.
///
/// CAS must be synced first so every referenced blob is durable before the
/// metadata transaction is committed.
pub async fn commit<M: MetaStore>(
    meta: &mut M,
    previous: &Previous,
    staged: &Assembled,
    created_at_ms: u64,
) -> Result<BackupReport, M::Error> {
    let carry_from = previous
        .snapshot
        .map(|snapshot| (snapshot.id, staged.carries.as_slice()));
    let outcome: ApplyOutcome = meta
        .apply_snapshot_incremental(
            created_at_ms,
            &staged.entries,
            carry_from,
            &staged.fingerprints,
            &staged.removed,
        )
        .await?;
    Ok(BackupReport {
        snapshot: outcome.id,
        chunks: staged.chunks,
        new_blobs: staged.new_blobs,
        tombstones: staged.tombstones,
        skipped_regions: staged.carries.len(),
        carried_chunks: outcome.carried_chunks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::MemMeta;
    use sekai_util::{ChunkHistoryEntry, Dimension, RegionKind};

    const OVER: Dimension = Dimension::OVERWORLD;
    const NETHER: Dimension = Dimension::NETHER;
    const REGION: RegionKind = RegionKind::REGION;

    const fn key(rx: i32, rz: i32) -> RegionKey {
        RegionKey::new(OVER, REGION, rx, rz)
    }

    const fn coord(x: i32, z: i32) -> ChunkCoord {
        ChunkCoord::new(OVER, REGION, x, z)
    }

    const fn nether_key(rx: i32, rz: i32) -> RegionKey {
        RegionKey::new(NETHER, REGION, rx, rz)
    }

    const fn nether_coord(x: i32, z: i32) -> ChunkCoord {
        ChunkCoord::new(NETHER, REGION, x, z)
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
        let (previous, plan) =
            crate::support::block_on(plan_backup(&meta, &observed, Scope::World)).unwrap();
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
        crate::support::block_on(meta.apply_snapshot_incremental(
            1_000,
            &[
                stage_present(coord(0, 0), BlobHash([1; 32])),
                stage_present(coord(32, 0), BlobHash([2; 32])),
            ],
            None,
            &[fp0, fp1],
            &[],
        ))
        .unwrap();

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
        let (previous, plan) =
            crate::support::block_on(plan_backup(&meta, &observed, Scope::World)).unwrap();
        assert_eq!(plan.carries, alloc::vec![key(1, 0)]);
        assert_eq!(plan.ingest, alloc::vec![key(0, 0)]);
        assert!(plan.removed.is_empty());

        let staged = assemble(
            plan,
            &previous,
            alloc::vec![stage_present(coord(0, 0), BlobHash([9; 32]))],
            BTreeSet::from([coord(0, 0)]),
            1,
            alloc::vec![changed],
            Scope::World,
        );
        assert_eq!(staged.chunks, 2);
        assert_eq!(staged.tombstones, 0);

        let report =
            crate::support::block_on(commit(&mut meta, &previous, &staged, 2_000)).unwrap();
        assert_eq!(report.chunks, 2);
        assert_eq!(report.new_blobs, 1);
        assert_eq!(report.skipped_regions, 1);
        assert_eq!(report.carried_chunks, 1);
        assert_eq!(report.tombstones, 0);

        let got = crate::support::block_on(meta.lookup_chunk(report.snapshot, &coord(0, 0)))
            .unwrap()
            .unwrap();
        assert_eq!(got.blob, Some(BlobHash([9; 32])));
        let kept = crate::support::block_on(meta.lookup_chunk(report.snapshot, &coord(32, 0)))
            .unwrap()
            .unwrap();
        assert_eq!(kept.blob, Some(BlobHash([2; 32])));
    }

    #[test]
    fn missing_coordinates_become_tombstones_and_states_drop() {
        let mut meta = MemMeta::default();
        let s1 = crate::support::block_on(meta.apply_snapshot_incremental(
            1_000,
            &[
                stage_present(coord(0, 0), BlobHash([1; 32])),
                stage_present(coord(1, 0), BlobHash([2; 32])),
            ],
            None,
            &[fingerprint(key(0, 0))],
            &[],
        ))
        .unwrap()
        .id;
        assert_eq!(s1, SnapshotId(1));

        let (previous, plan) =
            crate::support::block_on(plan_backup(&meta, &[], Scope::World)).unwrap();
        assert_eq!(plan.removed, alloc::vec![key(0, 0)]);
        let staged = assemble(
            plan,
            &previous,
            alloc::vec::Vec::new(),
            BTreeSet::new(),
            0,
            alloc::vec::Vec::new(),
            Scope::World,
        );
        assert_eq!(staged.chunks, 0);
        assert_eq!(staged.tombstones, 2);

        let report =
            crate::support::block_on(commit(&mut meta, &previous, &staged, 2_000)).unwrap();
        assert_eq!(report.tombstones, 2);
        assert!(meta.states.is_empty());
        let tomb = crate::support::block_on(meta.lookup_chunk(report.snapshot, &coord(0, 0)))
            .unwrap()
            .unwrap();
        assert!(tomb.is_tombstone());
    }

    #[test]
    fn scoped_plan_ignores_out_of_scope_regions() {
        let mut meta = MemMeta::default();
        let fp_over = fingerprint(key(0, 0));
        let fp_nether = RegionFingerprint {
            key: nether_key(0, 0),
            ..fingerprint(key(0, 0))
        };
        crate::support::block_on(meta.apply_snapshot_incremental(
            1_000,
            &[
                stage_present(coord(0, 0), BlobHash([1; 32])),
                stage_present(nether_coord(0, 0), BlobHash([2; 32])),
            ],
            None,
            &[fp_over, fp_nether],
            &[],
        ))
        .unwrap();

        // Only the nether region is observed (overworld file deleted from
        // disk), but the overworld scope sees neither ingest nor removal.
        let observed = [Observation {
            key: nether_key(0, 0),
            fingerprint: fp_nether,
        }];
        let scope = Scope::Dimension(NETHER);
        let (_, plan) = crate::support::block_on(plan_backup(&meta, &observed, scope)).unwrap();
        assert_eq!(plan.carries, alloc::vec![nether_key(0, 0)]);
        assert!(plan.ingest.is_empty());
        assert!(plan.removed.is_empty());

        // Out-of-scope observations never enter the plan at all.
        let observed = [
            Observation {
                key: key(0, 0),
                fingerprint: fp_over,
            },
            Observation {
                key: nether_key(0, 0),
                fingerprint: fp_nether,
            },
        ];
        let (_, plan) =
            crate::support::block_on(plan_backup(&meta, &observed, Scope::World)).unwrap();
        assert_eq!(plan.carries, alloc::vec![key(0, 0), nether_key(0, 0)]);
    }

    #[test]
    fn scoped_assemble_tombstones_only_in_scope() {
        let mut meta = MemMeta::default();
        let fp_nether = RegionFingerprint {
            key: nether_key(0, 0),
            ..fingerprint(key(0, 0))
        };
        crate::support::block_on(meta.apply_snapshot_incremental(
            1_000,
            &[
                stage_present(coord(0, 0), BlobHash([1; 32])),
                stage_present(coord(1, 0), BlobHash([2; 32])),
                stage_present(nether_coord(0, 0), BlobHash([3; 32])),
            ],
            None,
            &[fingerprint(key(0, 0)), fp_nether],
            &[],
        ))
        .unwrap();

        // Scoped backup of the overworld re-ingests its (changed) region:
        // (1,0) is genuinely gone, the nether chunk is out of scope and
        // must not become a tombstone.
        let mut changed = fingerprint(key(0, 0));
        changed.size += 1;
        let observed = [Observation {
            key: key(0, 0),
            fingerprint: changed,
        }];
        let scope = Scope::Dimension(OVER);
        let (previous, plan) =
            crate::support::block_on(plan_backup(&meta, &observed, scope)).unwrap();
        assert_eq!(plan.ingest, alloc::vec![key(0, 0)]);
        assert!(plan.removed.is_empty());
        let staged = assemble(
            plan,
            &previous,
            alloc::vec![stage_present(coord(0, 0), BlobHash([9; 32]))],
            BTreeSet::from([coord(0, 0)]),
            1,
            alloc::vec::Vec::new(),
            scope,
        );
        assert_eq!(staged.tombstones, 1);
        assert_eq!(staged.chunks, 1);
        let tombstoned: alloc::vec::Vec<ChunkCoord> = staged
            .entries
            .iter()
            .filter(|entry| entry.blob.is_none())
            .map(|entry| entry.coord)
            .collect();
        assert_eq!(tombstoned, alloc::vec![coord(1, 0)]);

        let report =
            crate::support::block_on(commit(&mut meta, &previous, &staged, 2_000)).unwrap();
        assert_eq!(report.tombstones, 1);
        // The nether chunk still resolves through fallback, untouched.
        let kept =
            crate::support::block_on(meta.lookup_chunk(report.snapshot, &nether_coord(0, 0)))
                .unwrap()
                .unwrap();
        assert_eq!(kept.blob, Some(BlobHash([3; 32])));
    }

    #[test]
    fn mem_meta_satisfies_lookup_contract() {
        let mut meta = MemMeta::default();
        let id = crate::support::block_on(meta.create_snapshot(5)).unwrap();
        crate::support::block_on(meta.record_chunk(
            id,
            &coord(0, 0),
            Some(&BlobHash([3; 32])),
            None,
        ))
        .unwrap();
        let row = crate::support::block_on(meta.lookup_chunk(id, &coord(0, 0)))
            .unwrap()
            .unwrap();
        assert_eq!(
            row,
            ChunkHistoryEntry::new(coord(0, 0), id, Some(BlobHash([3; 32])), None)
        );
        assert!(
            crate::support::block_on(meta.lookup_snapshot(SnapshotId(999)))
                .unwrap()
                .is_none()
        );
    }
}
