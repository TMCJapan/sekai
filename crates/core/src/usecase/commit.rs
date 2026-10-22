//! Recording a world state as a commit, over abstract ports.
//!
//! Change detection (`diff`) is deliberately not computed here; parsing
//! every chunk would multiply scan cost for data no consumer reads yet.
//!
//! The policy is stated in ARCHITECTURE.md ("Delta History Storage" and
//! "Tombstones"): unchanged files carry no new rows, vanished coordinates
//! tombstone once, and everything else resolves through fallback. This
//! module is the orchestration split that keeps adapters out of `core`:
//!
//! ```text
//! plan_commit    read previous state, decide carry vs ingest (policy)
//!   -> adapter ingests changed regions (hash + CAS put per chunk)
//! assemble       merge ingested rows, carried presence, tombstones (pure)
//!   -> adapter flushes CAS (durability barrier before metadata)
//! record_commit  record the commit atomically (single metadata batch)
//! ```

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use crate::port::meta::MetaStore;
use sekai_util::{
    ApplyOutcome, BlobHash, ChunkCoord, Commit, CommitEntry, CommitId, RegionFingerprint,
    RegionKey, RegionStateEntry, Scope,
};

/// Outcome of one commit run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitReport {
    /// Newly recorded commit ID.
    pub commit: CommitId,
    /// Chunks present on disk in this commit.
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

/// Previous commit plus the state the walk needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Previous {
    /// The commit the new one continues: the target of the active ref,
    /// when the store has one (`None` on an unborn ref).
    pub commit: Option<Commit>,
    /// Effective present coordinates of the base commit, tombstones
    /// excluded (empty on an unborn ref).
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

/// Staged commit decision: what to ingest, carry, or drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Fingerprint-matched regions, carried from the previous commit.
    pub carries: Vec<RegionKey>,
    /// Changed regions the adapter must ingest.
    pub ingest: Vec<RegionKey>,
    /// Stored regions gone from disk (state rows leave with this commit).
    pub removed: Vec<RegionKey>,
    /// Discovered region identities (for dropping stale state).
    pub discovered: BTreeSet<RegionKey>,
}

/// Merged rows ready to commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assembled {
    /// Fresh history rows (ingested chunks plus tombstones).
    pub entries: Vec<CommitEntry>,
    /// Coordinates present on disk (ingested plus carried).
    pub chunks: usize,
    /// Blobs newly written to CAS.
    pub new_blobs: usize,
    /// Tombstone rows recorded for vanished chunks.
    pub tombstones: usize,
    /// Fresh fingerprints for ingested files the scope fully covers.
    pub fingerprints: Vec<RegionFingerprint>,
    /// Ingested files the scope covers only partly; their state rows drop.
    pub invalidated: Vec<RegionKey>,
    /// Fingerprint-matched regions, carried from the previous commit.
    pub carries: Vec<RegionKey>,
    /// Stored regions gone from disk.
    pub removed: Vec<RegionKey>,
}

/// Load prior state and classify observed regions as carried or ingested.
///
/// The base is the target of the active ref: a new commit continues the
/// history that ref names, not the newest commit in the store. A store
/// without an active ref (or with an unborn one) starts a new history.
///
/// The caller must hold the store's writer lock from this call through
/// [`record_commit`]: a commit that moves the active ref in between
/// would make the plan's base and the recorded parent disagree.
///
/// Regions outside `scope` are ignored entirely: they are neither carried
/// nor ingested, and their stored state rows are never marked removed.
/// Callers must quiesce the source while fingerprinting; a concurrent rewrite
/// after the match is not detected until a later commit.
pub async fn plan_commit<M: MetaStore>(
    meta: &M,
    observed: &[Observation],
    scope: &Scope,
) -> Result<(Previous, Plan), M::Error> {
    let active = meta.active_ref().await?;
    // `MetaStore::active_ref` rejects a dangling target (the port's
    // contract), so the lookup below always finds the commit the ref
    // names: a `None` here is an unborn ref.
    let base = match active.as_ref().and_then(|r| r.commit) {
        Some(id) => meta.lookup_commit(id).await?,
        None => None,
    };
    let mut universe: BTreeSet<ChunkCoord> = BTreeSet::new();
    if let Some(base) = base.as_ref() {
        meta.visit_commit_chunks(base.id, |entry| {
            // Present coordinates only. A tombstoned coordinate is already
            // absent from the effective state, so keeping it would re-record
            // the same tombstone row on every later commit and count a
            // vanished chunk as present.
            if entry.blob.is_some() {
                universe.insert(entry.coord);
            }
            true
        })
        .await?;
    }
    let mut states: BTreeMap<RegionKey, RegionStateEntry> = BTreeMap::new();
    // Only fingerprints confirmed inside the base's own history may be
    // carried: one confirmed in another history describes a different
    // state of the file.
    for state in meta.load_region_states(base.as_ref().map(|c| c.id)).await? {
        states.insert(state.fingerprint.key, state);
    }
    let previous = Previous {
        commit: base,
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
        let key = state.fingerprint.key;
        if scope.matches_region(key) && !plan.discovered.contains(&key) {
            plan.removed.push(key);
        }
    }
    Ok((previous, plan))
}

/// Stage an ingested chunk without computing its volatile diff hash.
pub const fn stage_present(coord: ChunkCoord, hash: BlobHash) -> CommitEntry {
    CommitEntry::new(coord, Some(hash), None)
}

/// What the adapter's ingest pass produced for the changed files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ingested {
    /// Fresh history rows for every chunk read.
    pub entries: Vec<CommitEntry>,
    /// Coordinates those rows cover.
    pub present: BTreeSet<ChunkCoord>,
    /// Blobs newly written to CAS.
    pub new_blobs: usize,
    /// Fresh fingerprints for the ingested files.
    pub fingerprints: Vec<RegionFingerprint>,
}

/// Merge fresh rows with carried coordinates and tombstones.
///
/// Previously present coordinates inside `scope` but not present in the new
/// scan become tombstones; out-of-scope coordinates keep resolving through
/// fallback, so scoped commits never record spurious tombstones. Coordinates
/// already tombstoned at the previous commit are absent from
/// `Previous::universe` and therefore neither carried nor re-tombstoned.
///
/// A stored fingerprint promises that the whole file matches the latest
/// history, which is what lets a later commit carry it unread. A partial
/// ingest records only the in-scope chunks, so its fingerprint is dropped
/// and any older state row is invalidated rather than left to match a
/// file whose history it no longer describes.
pub fn assemble(plan: Plan, previous: &Previous, ingested: Ingested, scope: &Scope) -> Assembled {
    let Ingested {
        mut entries,
        mut present,
        new_blobs,
        fingerprints,
    } = ingested;
    let (fingerprints, partial): (Vec<_>, Vec<_>) = fingerprints
        .into_iter()
        .partition(|fp| scope.covers_region(fp.key));
    // A carried file is still on disk, so its coordinates count as present
    // without being read again.
    if !plan.carries.is_empty() {
        let skipped: BTreeSet<RegionKey> = plan.carries.iter().copied().collect();
        for coord in &previous.universe {
            if scope.contains(*coord) && skipped.contains(&RegionKey::of(*coord)) {
                present.insert(*coord);
            }
        }
    }
    // Present before, absent from this scan: the coordinate just vanished.
    let mut tombstones = 0usize;
    for coord in &previous.universe {
        if scope.contains(*coord) && !present.contains(coord) {
            entries.push(CommitEntry::new(*coord, None, None));
            tombstones += 1;
        }
    }
    Assembled {
        chunks: present.len(),
        entries,
        new_blobs,
        tombstones,
        fingerprints,
        invalidated: partial.into_iter().map(|fp| fp.key).collect(),
        carries: plan.carries,
        removed: plan.removed,
    }
}

/// Record assembled rows as one commit.
///
/// CAS must be synced first so every referenced blob is durable before the
/// metadata transaction is committed.
///
/// The new commit continues the history it was planned against: its
/// parent is `previous.commit` - the target of the active ref, which is
/// also the base its delta was computed from. The active ref moves onto
/// the new commit in the same transaction. Plan and commit must not be
/// interleaved with another commit; the app holds the store's writer
/// lock for the whole run.
pub async fn record_commit<M: MetaStore>(
    meta: &mut M,
    previous: &Previous,
    staged: &Assembled,
    created_at_ms: u64,
) -> Result<CommitReport, M::Error> {
    let carry_from = previous
        .commit
        .as_ref()
        .map(|commit| (commit.id, staged.carries.as_slice()));
    let parents: Vec<CommitId> = previous
        .commit
        .as_ref()
        .map(|commit| alloc::vec![commit.id])
        .unwrap_or_default();
    // Both drop state rows; they stay apart in `Assembled` so previews
    // report only vanished files as deleted.
    let dropped_states: Vec<RegionKey> = staged
        .removed
        .iter()
        .chain(&staged.invalidated)
        .copied()
        .collect();
    let outcome: ApplyOutcome = meta
        .apply_commit_incremental(
            created_at_ms,
            &parents,
            &staged.entries,
            carry_from,
            &staged.fingerprints,
            &dropped_states,
        )
        .await?;
    Ok(CommitReport {
        commit: outcome.id,
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
    use crate::support::{MemError, MemMeta};
    use sekai_util::{Area, ChunkHistoryEntry, Dimension, Rect, RefName, RegionKind};

    const OVER: Dimension = Dimension::OVERWORLD;
    const NETHER: Dimension = Dimension::NETHER;
    const REGION: RegionKind = RegionKind::Region;

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

    /// The store's active ref: new commits hang from it.
    fn main_ref() -> RefName {
        RefName::parse("main").unwrap()
    }

    /// A fake store with one active ref, the way a fresh store looks.
    fn store_with_ref() -> MemMeta {
        let mut meta = MemMeta::default();
        meta.with_active_ref(&main_ref());
        meta
    }

    fn fingerprint(key: RegionKey) -> RegionFingerprint {
        RegionFingerprint {
            key,
            mtime_ms: Some(1_700_000_000_000),
            size: 8192,
            content_hash: [7; 32],
        }
    }

    #[test]
    fn first_commit_ingests_everything() {
        let meta = store_with_ref();
        let observed = [Observation {
            key: key(0, 0),
            fingerprint: fingerprint(key(0, 0)),
        }];
        let (previous, plan) =
            crate::support::block_on(plan_commit(&meta, &observed, &Scope::World)).unwrap();
        assert!(previous.commit.is_none());
        assert!(previous.universe.is_empty());
        assert_eq!(plan.carries, [] as [sekai_util::RegionKey; 0]);
        assert_eq!(plan.ingest, alloc::vec![key(0, 0)]);
        assert_eq!(plan.removed, [] as [sekai_util::RegionKey; 0]);
    }

    #[test]
    fn a_new_commit_continues_the_active_ref() {
        let mut meta = store_with_ref();
        let main = main_ref();
        let observed = [Observation {
            key: key(0, 0),
            fingerprint: fingerprint(key(0, 0)),
        }];

        // The active ref is unborn: the commit starts a history.
        let (previous, plan) =
            crate::support::block_on(plan_commit(&meta, &observed, &Scope::World)).unwrap();
        assert!(previous.commit.is_none());
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: alloc::vec![stage_present(coord(0, 0), BlobHash([1; 32]))],
                present: BTreeSet::from([coord(0, 0)]),
                new_blobs: 1,
                fingerprints: alloc::vec![fingerprint(key(0, 0))],
            },
            &Scope::World,
        );
        let first =
            crate::support::block_on(record_commit(&mut meta, &previous, &staged, 1_000)).unwrap();
        // The ref moved onto the new commit.
        let active = crate::support::block_on(meta.active_ref())
            .unwrap()
            .unwrap();
        assert_eq!(active.commit, Some(first.commit));

        // A second ref at the same commit forks the history: the next
        // commit continues the ref that is active.
        let feature = RefName::parse("feature").unwrap();
        crate::support::block_on(meta.set_ref(&feature, Some(first.commit), 1_500)).unwrap();
        crate::support::block_on(meta.activate_ref(&feature)).unwrap();

        let (previous, plan) =
            crate::support::block_on(plan_commit(&meta, &observed, &Scope::World)).unwrap();
        assert_eq!(previous.commit.as_ref().map(|c| c.id), Some(first.commit));
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: alloc::vec![stage_present(coord(0, 0), BlobHash([2; 32]))],
                present: BTreeSet::from([coord(0, 0)]),
                new_blobs: 1,
                fingerprints: alloc::vec![fingerprint(key(0, 0))],
            },
            &Scope::World,
        );
        let second =
            crate::support::block_on(record_commit(&mut meta, &previous, &staged, 2_000)).unwrap();
        assert_eq!(
            crate::support::block_on(meta.commit_parents(second.commit)).unwrap(),
            alloc::vec![first.commit]
        );
        // The ref that was left behind keeps naming the first commit.
        let main = crate::support::block_on(meta.lookup_ref(&main))
            .unwrap()
            .unwrap();
        assert_eq!(main.commit, Some(first.commit));
    }

    #[test]
    fn the_recorded_commit_continues_the_previous_one() {
        let mut meta = store_with_ref();
        let observed = [Observation {
            key: key(0, 0),
            fingerprint: fingerprint(key(0, 0)),
        }];

        // The first commit starts a history: no previous commit, no
        // parent.
        let (previous, plan) =
            crate::support::block_on(plan_commit(&meta, &observed, &Scope::World)).unwrap();
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: alloc::vec![stage_present(coord(0, 0), BlobHash([1; 32]))],
                present: BTreeSet::from([coord(0, 0)]),
                new_blobs: 1,
                fingerprints: alloc::vec![fingerprint(key(0, 0))],
            },
            &Scope::World,
        );
        let first =
            crate::support::block_on(record_commit(&mut meta, &previous, &staged, 1_000)).unwrap();
        assert_eq!(
            crate::support::block_on(meta.commit_parents(first.commit)).unwrap(),
            alloc::vec![]
        );

        // The second commit continues the history of the commit it was
        // planned against.
        let (previous, plan) =
            crate::support::block_on(plan_commit(&meta, &observed, &Scope::World)).unwrap();
        assert_eq!(previous.commit.as_ref().map(|c| c.id), Some(first.commit));
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: alloc::vec![stage_present(coord(0, 0), BlobHash([2; 32]))],
                present: BTreeSet::from([coord(0, 0)]),
                new_blobs: 1,
                fingerprints: alloc::vec![fingerprint(key(0, 0))],
            },
            &Scope::World,
        );
        let second =
            crate::support::block_on(record_commit(&mut meta, &previous, &staged, 2_000)).unwrap();
        assert_eq!(
            crate::support::block_on(meta.commit_parents(second.commit)).unwrap(),
            alloc::vec![first.commit]
        );
    }

    #[test]
    fn a_commit_must_continue_the_active_ref() {
        let mut meta = store_with_ref();
        let first = crate::support::block_on(meta.apply_commit_incremental(
            1_000,
            &[],
            &[stage_present(coord(0, 0), BlobHash([1; 32]))],
            None,
            &[],
            &[],
        ))
        .unwrap()
        .id;

        // An empty parent list on a born ref would move the active ref
        // into an unrelated history.
        let err = crate::support::block_on(meta.apply_commit_incremental(
            2_000,
            &[],
            &[stage_present(coord(1, 0), BlobHash([2; 32]))],
            None,
            &[],
            &[],
        ))
        .unwrap_err();
        assert_eq!(err, MemError::NotContinuing { base: first.0 });

        // The rejected call wrote nothing and consumed no identifier.
        let second = crate::support::block_on(meta.apply_commit_incremental(
            3_000,
            &[first],
            &[stage_present(coord(1, 0), BlobHash([2; 32]))],
            None,
            &[],
            &[],
        ))
        .unwrap()
        .id;
        assert_eq!(second, CommitId(2));
        assert_eq!(
            crate::support::block_on(meta.lookup_ref(&main_ref()))
                .unwrap()
                .unwrap()
                .commit,
            Some(second)
        );
    }

    #[test]
    fn changed_region_reingests_while_rest_carries() {
        let mut meta = store_with_ref();
        let fp0 = fingerprint(key(0, 0));
        let fp1 = fingerprint(key(1, 0));
        crate::support::block_on(meta.apply_commit_incremental(
            1_000,
            &[],
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
            crate::support::block_on(plan_commit(&meta, &observed, &Scope::World)).unwrap();
        assert_eq!(plan.carries, alloc::vec![key(1, 0)]);
        assert_eq!(plan.ingest, alloc::vec![key(0, 0)]);
        assert_eq!(plan.removed, [] as [sekai_util::RegionKey; 0]);

        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: alloc::vec![stage_present(coord(0, 0), BlobHash([9; 32]))],
                present: BTreeSet::from([coord(0, 0)]),
                new_blobs: 1,
                fingerprints: alloc::vec![changed],
            },
            &Scope::World,
        );
        assert_eq!(staged.chunks, 2);
        assert_eq!(staged.tombstones, 0);

        let report =
            crate::support::block_on(record_commit(&mut meta, &previous, &staged, 2_000)).unwrap();
        assert_eq!(report.chunks, 2);
        assert_eq!(report.new_blobs, 1);
        assert_eq!(report.skipped_regions, 1);
        assert_eq!(report.carried_chunks, 1);
        assert_eq!(report.tombstones, 0);

        let got = crate::support::block_on(meta.lookup_chunk(report.commit, &coord(0, 0)))
            .unwrap()
            .unwrap();
        assert_eq!(got.blob, Some(BlobHash([9; 32])));
        let kept = crate::support::block_on(meta.lookup_chunk(report.commit, &coord(32, 0)))
            .unwrap()
            .unwrap();
        assert_eq!(kept.blob, Some(BlobHash([2; 32])));
    }

    #[test]
    fn missing_coordinates_become_tombstones_and_states_drop() {
        let mut meta = store_with_ref();
        let s1 = crate::support::block_on(meta.apply_commit_incremental(
            1_000,
            &[],
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
        assert_eq!(s1, CommitId(1));

        let (previous, plan) =
            crate::support::block_on(plan_commit(&meta, &[], &Scope::World)).unwrap();
        assert_eq!(plan.removed, alloc::vec![key(0, 0)]);
        let staged = assemble(plan, &previous, Ingested::default(), &Scope::World);
        assert_eq!(staged.chunks, 0);
        assert_eq!(staged.tombstones, 2);

        let report =
            crate::support::block_on(record_commit(&mut meta, &previous, &staged, 2_000)).unwrap();
        assert_eq!(report.tombstones, 2);
        assert_eq!(meta.states, [] as [sekai_util::RegionStateEntry; 0]);
        let tomb = crate::support::block_on(meta.lookup_chunk(report.commit, &coord(0, 0)))
            .unwrap()
            .unwrap();
        assert!(tomb.is_tombstone());
    }

    #[test]
    fn already_tombstoned_coordinates_record_no_further_rows() {
        let mut meta = store_with_ref();
        let fp0 = fingerprint(key(0, 0));
        let fp1 = fingerprint(key(1, 0));
        crate::support::block_on(meta.apply_commit_incremental(
            1_000,
            &[],
            &[
                stage_present(coord(0, 0), BlobHash([1; 32])),
                stage_present(coord(32, 0), BlobHash([2; 32])),
            ],
            None,
            &[fp0, fp1],
            &[],
        ))
        .unwrap();

        // Region (0,0) is gone from disk: its chunk tombstones once while
        // region (1,0) carries over.
        let (previous, plan) = crate::support::block_on(plan_commit(
            &meta,
            &[Observation {
                key: key(1, 0),
                fingerprint: fp1,
            }],
            &Scope::World,
        ))
        .unwrap();
        let staged = assemble(plan, &previous, Ingested::default(), &Scope::World);
        assert_eq!(staged.tombstones, 1);
        assert_eq!(staged.chunks, 1);
        let report =
            crate::support::block_on(record_commit(&mut meta, &previous, &staged, 2_000)).unwrap();
        assert_eq!(report.tombstones, 1);
        assert_eq!(report.chunks, 1);
        assert_eq!(report.carried_chunks, 1);

        // Repeat commit with the same world: the tombstone already resolves
        // through fallback, so it records no fresh row and the vanished
        // chunk is not carried back as present.
        let (previous, plan) = crate::support::block_on(plan_commit(
            &meta,
            &[Observation {
                key: key(1, 0),
                fingerprint: fp1,
            }],
            &Scope::World,
        ))
        .unwrap();
        assert_eq!(previous.universe, BTreeSet::from([coord(32, 0)]));
        let staged = assemble(plan, &previous, Ingested::default(), &Scope::World);
        assert_eq!(staged.entries, [] as [sekai_util::CommitEntry; 0]);
        assert_eq!(staged.tombstones, 0);
        assert_eq!(staged.chunks, 1);

        let report =
            crate::support::block_on(record_commit(&mut meta, &previous, &staged, 3_000)).unwrap();
        assert_eq!(report.tombstones, 0);
        assert_eq!(report.chunks, 1);
        assert!(meta.rows.iter().all(|row| row.commit != report.commit));
    }

    #[test]
    fn scoped_plan_ignores_out_of_scope_regions() {
        let mut meta = store_with_ref();
        let fp_over = fingerprint(key(0, 0));
        let fp_nether = RegionFingerprint {
            key: nether_key(0, 0),
            ..fingerprint(key(0, 0))
        };
        crate::support::block_on(meta.apply_commit_incremental(
            1_000,
            &[],
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
        let scope = Scope::dimension(NETHER);
        let (_, plan) = crate::support::block_on(plan_commit(&meta, &observed, &scope)).unwrap();
        assert_eq!(plan.carries, alloc::vec![nether_key(0, 0)]);
        assert_eq!(plan.ingest, [] as [sekai_util::RegionKey; 0]);
        assert_eq!(plan.removed, [] as [sekai_util::RegionKey; 0]);

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
            crate::support::block_on(plan_commit(&meta, &observed, &Scope::World)).unwrap();
        assert_eq!(plan.carries, alloc::vec![key(0, 0), nether_key(0, 0)]);
    }

    #[test]
    fn scoped_assemble_tombstones_only_in_scope() {
        let mut meta = store_with_ref();
        let fp_nether = RegionFingerprint {
            key: nether_key(0, 0),
            ..fingerprint(key(0, 0))
        };
        crate::support::block_on(meta.apply_commit_incremental(
            1_000,
            &[],
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

        // Scoped commit of the overworld re-ingests its (changed) region:
        // (1,0) is genuinely gone, the nether chunk is out of scope and
        // must not become a tombstone.
        let mut changed = fingerprint(key(0, 0));
        changed.size += 1;
        let observed = [Observation {
            key: key(0, 0),
            fingerprint: changed,
        }];
        let scope = Scope::dimension(OVER);
        let (previous, plan) =
            crate::support::block_on(plan_commit(&meta, &observed, &scope)).unwrap();
        assert_eq!(plan.ingest, alloc::vec![key(0, 0)]);
        assert_eq!(plan.removed, [] as [sekai_util::RegionKey; 0]);
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: alloc::vec![stage_present(coord(0, 0), BlobHash([9; 32]))],
                present: BTreeSet::from([coord(0, 0)]),
                new_blobs: 1,
                fingerprints: alloc::vec::Vec::new(),
            },
            &scope,
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
            crate::support::block_on(record_commit(&mut meta, &previous, &staged, 2_000)).unwrap();
        assert_eq!(report.tombstones, 1);
        // The nether chunk still resolves through fallback, untouched.
        let kept = crate::support::block_on(meta.lookup_chunk(report.commit, &nether_coord(0, 0)))
            .unwrap()
            .unwrap();
        assert_eq!(kept.blob, Some(BlobHash([3; 32])));
    }

    fn over_rect(rect: Rect) -> Scope {
        Scope::Select {
            kinds: alloc::vec![REGION],
            areas: alloc::vec![(OVER, Area::Rect(rect))],
        }
    }

    #[test]
    fn partial_ingest_invalidates_state_so_full_commit_reingests() {
        let mut meta = store_with_ref();
        let fp0 = fingerprint(key(0, 0));
        crate::support::block_on(meta.apply_commit_incremental(
            1_000,
            &[],
            &[
                stage_present(coord(0, 0), BlobHash([1; 32])),
                stage_present(coord(1, 0), BlobHash([2; 32])),
            ],
            None,
            &[fp0],
            &[],
        ))
        .unwrap();

        // Both chunks changed on disk, but the scope reads only (0,0).
        let mut changed = fp0;
        changed.size += 1;
        let observed = [Observation {
            key: key(0, 0),
            fingerprint: changed,
        }];
        let scope = over_rect(Rect::new(0, 0, 0, 0));
        let (previous, plan) =
            crate::support::block_on(plan_commit(&meta, &observed, &scope)).unwrap();
        assert_eq!(plan.ingest, alloc::vec![key(0, 0)]);
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: alloc::vec![stage_present(coord(0, 0), BlobHash([9; 32]))],
                present: BTreeSet::from([coord(0, 0)]),
                new_blobs: 1,
                fingerprints: alloc::vec![changed],
            },
            &scope,
        );
        assert_eq!(staged.fingerprints, [] as [RegionFingerprint; 0]);
        assert_eq!(staged.invalidated, alloc::vec![key(0, 0)]);
        assert_eq!(staged.removed, [] as [RegionKey; 0]);
        assert_eq!(staged.tombstones, 0);
        crate::support::block_on(record_commit(&mut meta, &previous, &staged, 2_000)).unwrap();
        assert_eq!(meta.states, [] as [RegionStateEntry; 0]);

        // The unread (1,0) change must not be carried as unchanged.
        let (_, plan) =
            crate::support::block_on(plan_commit(&meta, &observed, &Scope::World)).unwrap();
        assert_eq!(plan.carries, [] as [RegionKey; 0]);
        assert_eq!(plan.ingest, alloc::vec![key(0, 0)]);
    }

    #[test]
    fn covering_scope_refreshes_the_fingerprint_so_later_commits_carry() {
        let mut meta = store_with_ref();
        let fp0 = fingerprint(key(0, 0));
        crate::support::block_on(meta.apply_commit_incremental(
            1_000,
            &[],
            &[stage_present(coord(0, 0), BlobHash([1; 32]))],
            None,
            &[fp0],
            &[],
        ))
        .unwrap();

        let mut changed = fp0;
        changed.size += 1;
        let observed = [Observation {
            key: key(0, 0),
            fingerprint: changed,
        }];
        let scope = over_rect(Rect::region(0, 0));
        let (previous, plan) =
            crate::support::block_on(plan_commit(&meta, &observed, &scope)).unwrap();
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: alloc::vec![stage_present(coord(0, 0), BlobHash([9; 32]))],
                present: BTreeSet::from([coord(0, 0)]),
                new_blobs: 1,
                fingerprints: alloc::vec![changed],
            },
            &scope,
        );
        assert_eq!(staged.fingerprints, alloc::vec![changed]);
        assert_eq!(staged.invalidated, [] as [RegionKey; 0]);
        crate::support::block_on(record_commit(&mut meta, &previous, &staged, 2_000)).unwrap();
        let fingerprints: alloc::vec::Vec<RegionFingerprint> =
            meta.states.iter().map(|state| state.fingerprint).collect();
        assert_eq!(fingerprints, alloc::vec![changed]);

        let (_, plan) =
            crate::support::block_on(plan_commit(&meta, &observed, &Scope::World)).unwrap();
        assert_eq!(plan.carries, alloc::vec![key(0, 0)]);
        assert_eq!(plan.ingest, [] as [RegionKey; 0]);
    }

    #[test]
    fn mem_meta_satisfies_lookup_contract() {
        let mut meta = store_with_ref();
        let id = crate::support::block_on(meta.create_commit(5, &[])).unwrap();
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
            crate::support::block_on(meta.lookup_commit(CommitId(999)))
                .unwrap()
                .is_none()
        );
    }
}
