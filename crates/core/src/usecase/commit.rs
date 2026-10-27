//! Recording a world state as a commit, over abstract ports.
//!
//! Change detection (`diff`) is deliberately not computed here; parsing
//! every chunk would multiply scan cost for data no consumer reads yet.
//!
//! The policy is stated in ARCHITECTURE.md ("Delta History Storage" and
//! "Tombstones"): chunks unchanged since the base commit write no fresh
//! rows, vanished coordinates tombstone once, and everything else resolves
//! through fallback. This module is the orchestration split that keeps
//! adapters out of `core`:
//!
//! ```text
//! plan_commit    read the base state and list in-scope regions to ingest
//!   -> adapter ingests every in-scope region (hash + CAS put per chunk)
//! assemble       drop unchanged rows, add tombstones (pure)
//!   -> adapter flushes CAS (durability barrier before metadata)
//! record_commit  record the commit atomically (single metadata batch)
//! ```

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use crate::port::meta::MetaStore;
use sekai_util::{
    ApplyOutcome, BlobHash, ChunkCoord, ChunkHistoryEntry, Commit, CommitEntry, CommitId,
    RegionFingerprint, RegionKey, Scope,
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
    /// Region files skipped via fingerprint match. Always zero: region
    /// skipping was removed together with the derived-state table.
    pub skipped_regions: usize,
    /// History rows carried over for skipped regions. Always zero.
    pub carried_chunks: usize,
}

/// Previous commit plus the state the walk needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Previous {
    /// The commit the new one continues: the target of the active ref,
    /// when the store has one (`None` on an unborn ref).
    pub commit: Option<Commit>,
    /// Effective present rows of the base commit, tombstones excluded
    /// (empty on an unborn ref). Keyed by coordinate; the row carries the
    /// blob hash used for chunk-level change detection.
    pub state: BTreeMap<ChunkCoord, ChunkHistoryEntry>,
}

/// One region file observed on disk by the adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observation {
    /// Which region file was observed.
    pub key: RegionKey,
    /// Fresh fingerprint of the file. Kept for diagnostics and timing;
    /// commit decisions compare chunk hashes instead of file metadata.
    pub fingerprint: RegionFingerprint,
}

/// Staged commit decision: what to ingest or report as gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// In-scope regions the adapter must ingest.
    pub ingest: Vec<RegionKey>,
    /// Regions with present chunks at the base but no file on disk now.
    pub removed: Vec<RegionKey>,
    /// Discovered region identities inside the scope.
    pub discovered: BTreeSet<RegionKey>,
}

/// Merged rows ready to commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assembled {
    /// Fresh history rows (changed chunks plus tombstones).
    pub entries: Vec<CommitEntry>,
    /// Coordinates present on disk.
    pub chunks: usize,
    /// Blobs newly written to CAS.
    pub new_blobs: usize,
    /// Tombstone rows recorded for vanished chunks.
    pub tombstones: usize,
    /// In-scope files with no present chunk at the base commit.
    pub new_files: usize,
    /// Stored regions gone from disk.
    pub removed: Vec<RegionKey>,
}

/// Load prior state and list the in-scope regions to ingest.
///
/// The base is the target of the active ref: a new commit continues the
/// history that ref names, not the newest commit in the store. A store
/// without an active ref (or with an unborn one) starts a new history.
///
/// The caller must hold the store's writer lock from this call through
/// [`record_commit`]: a commit that moves the active ref in between
/// would make the plan's base and the recorded parent disagree.
///
/// Regions outside `scope` are ignored entirely: they are neither ingested
/// nor reported as removed. Callers must quiesce the source while reading;
/// a concurrent rewrite after the read is not detected until a later
/// commit.
///
/// Every in-scope file is ingested: persisted file fingerprints were
/// removed with the derived-state table, so skipping is impossible without
/// rereading the file. Chunk-level delta writes are preserved by
/// [`assemble`], which drops rows whose blob matches the base commit.
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
    let mut state: BTreeMap<ChunkCoord, ChunkHistoryEntry> = BTreeMap::new();
    if let Some(base) = base.as_ref() {
        meta.visit_commit_chunks(base.id, |entry| {
            // Present rows only. A tombstoned coordinate is already absent
            // from the effective state, so keeping it would re-record the
            // same tombstone row on every later commit and count a vanished
            // chunk as present.
            if entry.blob.is_some() {
                state.insert(entry.coord, *entry);
            }
            true
        })
        .await?;
    }
    let previous = Previous {
        commit: base,
        state,
    };
    let mut plan = Plan {
        ingest: Vec::new(),
        removed: Vec::new(),
        discovered: BTreeSet::new(),
    };
    for obs in observed {
        if scope.matches_region(obs.key) {
            plan.discovered.insert(obs.key);
            plan.ingest.push(obs.key);
        }
    }
    let mut removed: BTreeSet<RegionKey> = BTreeSet::new();
    for coord in previous.state.keys() {
        let key = RegionKey::of(*coord);
        if scope.matches_region(key) && !plan.discovered.contains(&key) {
            removed.insert(key);
        }
    }
    plan.removed = removed.into_iter().collect();
    Ok((previous, plan))
}

/// Stage an ingested chunk without computing its volatile diff hash.
pub const fn stage_present(coord: ChunkCoord, hash: BlobHash) -> CommitEntry {
    CommitEntry::new(coord, Some(hash), None)
}

/// What the adapter's ingest pass produced for the in-scope files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ingested {
    /// Fresh history rows for every chunk read.
    pub entries: Vec<CommitEntry>,
    /// Coordinates those rows cover.
    pub present: BTreeSet<ChunkCoord>,
    /// Blobs newly written to CAS.
    pub new_blobs: usize,
}

/// Merge fresh rows with tombstones.
///
/// Previously present coordinates inside `scope` but not present in the new
/// scan become tombstones; out-of-scope coordinates keep resolving through
/// fallback, so scoped commits never record spurious tombstones. Coordinates
/// already tombstoned at the previous commit are absent from
/// `Previous::state` and therefore never re-tombstoned.
///
/// Rows whose blob matches the base commit are dropped: the file was
/// reread, but its content did not change, so writing the row again would
/// bloat history without changing the effective state. The diff column is a
/// volatile cache and does not participate in the comparison.
pub fn assemble(plan: Plan, previous: &Previous, ingested: Ingested, scope: &Scope) -> Assembled {
    let Ingested {
        entries,
        present,
        new_blobs,
    } = ingested;
    let new_files = plan
        .ingest
        .iter()
        .filter(|key| {
            !previous
                .state
                .keys()
                .any(|coord| RegionKey::of(*coord) == **key)
        })
        .count();
    let mut fresh: Vec<CommitEntry> = entries
        .into_iter()
        .filter(|entry| {
            previous
                .state
                .get(&entry.coord)
                .is_none_or(|base| base.blob != entry.blob)
        })
        .collect();
    // Present before, absent from this scan: the coordinate just vanished.
    let mut tombstones = 0usize;
    for coord in previous.state.keys() {
        if scope.contains(*coord) && !present.contains(coord) {
            fresh.push(CommitEntry::new(*coord, None, None));
            tombstones += 1;
        }
    }
    Assembled {
        chunks: present.len(),
        entries: fresh,
        new_blobs,
        tombstones,
        new_files,
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
    let parents: Vec<CommitId> = previous
        .commit
        .as_ref()
        .map(|commit| alloc::vec![commit.id])
        .unwrap_or_default();
    let outcome: ApplyOutcome = meta
        .apply_commit_incremental(created_at_ms, &parents, &staged.entries)
        .await?;
    Ok(CommitReport {
        commit: outcome.id,
        chunks: staged.chunks,
        new_blobs: staged.new_blobs,
        tombstones: staged.tombstones,
        skipped_regions: 0,
        carried_chunks: 0,
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
        assert!(previous.state.is_empty());
        assert_eq!(plan.ingest, alloc::vec![key(0, 0)]);
        assert_eq!(plan.removed, [] as [RegionKey; 0]);
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
        ))
        .unwrap()
        .id;

        // An empty parent list on a born ref would move the active ref
        // into an unrelated history.
        let err = crate::support::block_on(meta.apply_commit_incremental(
            2_000,
            &[],
            &[stage_present(coord(1, 0), BlobHash([2; 32]))],
        ))
        .unwrap_err();
        assert_eq!(err, MemError::NotContinuing { base: first });

        // The rejected call wrote nothing.
        let second = crate::support::block_on(meta.apply_commit_incremental(
            3_000,
            &[first],
            &[stage_present(coord(1, 0), BlobHash([2; 32]))],
        ))
        .unwrap()
        .id;
        assert_ne!(second, first);
        assert_eq!(
            crate::support::block_on(meta.lookup_ref(&main_ref()))
                .unwrap()
                .unwrap()
                .commit,
            Some(second)
        );
    }

    #[test]
    fn unchanged_chunks_write_no_rows() {
        let mut meta = store_with_ref();
        let observed = [Observation {
            key: key(0, 0),
            fingerprint: fingerprint(key(0, 0)),
        }];
        let entries = [
            stage_present(coord(0, 0), BlobHash([1; 32])),
            stage_present(coord(1, 0), BlobHash([2; 32])),
        ];
        let present = BTreeSet::from([coord(0, 0), coord(1, 0)]);
        let (previous, plan) =
            crate::support::block_on(plan_commit(&meta, &observed, &Scope::World)).unwrap();
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: entries.to_vec(),
                present: present.clone(),
                new_blobs: 2,
            },
            &Scope::World,
        );
        let first =
            crate::support::block_on(record_commit(&mut meta, &previous, &staged, 1_000)).unwrap();

        let (previous, plan) =
            crate::support::block_on(plan_commit(&meta, &observed, &Scope::World)).unwrap();
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: entries.to_vec(),
                present,
                new_blobs: 0,
            },
            &Scope::World,
        );
        assert_eq!(staged.entries, [] as [CommitEntry; 0]);
        assert_eq!(staged.chunks, 2);
        let report =
            crate::support::block_on(record_commit(&mut meta, &previous, &staged, 2_000)).unwrap();
        assert_ne!(report.commit, first.commit);
        assert_eq!(report.new_blobs, 0);
        assert_eq!(report.tombstones, 0);
        assert!(meta.rows.iter().all(|row| row.commit != report.commit));
    }

    #[test]
    fn changed_chunk_writes_only_its_row() {
        let mut meta = store_with_ref();
        let observed = [Observation {
            key: key(0, 0),
            fingerprint: fingerprint(key(0, 0)),
        }];
        let (previous, plan) =
            crate::support::block_on(plan_commit(&meta, &observed, &Scope::World)).unwrap();
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: alloc::vec![
                    stage_present(coord(0, 0), BlobHash([1; 32])),
                    stage_present(coord(1, 0), BlobHash([2; 32])),
                ],
                present: BTreeSet::from([coord(0, 0), coord(1, 0)]),
                new_blobs: 2,
            },
            &Scope::World,
        );
        crate::support::block_on(record_commit(&mut meta, &previous, &staged, 1_000)).unwrap();

        let (previous, plan) =
            crate::support::block_on(plan_commit(&meta, &observed, &Scope::World)).unwrap();
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: alloc::vec![
                    stage_present(coord(0, 0), BlobHash([9; 32])),
                    stage_present(coord(1, 0), BlobHash([2; 32])),
                ],
                present: BTreeSet::from([coord(0, 0), coord(1, 0)]),
                new_blobs: 1,
            },
            &Scope::World,
        );
        assert_eq!(
            staged.entries,
            alloc::vec![stage_present(coord(0, 0), BlobHash([9; 32]))]
        );
        assert_eq!(staged.chunks, 2);
        let report =
            crate::support::block_on(record_commit(&mut meta, &previous, &staged, 2_000)).unwrap();
        assert_eq!(report.chunks, 2);
        assert_eq!(report.new_blobs, 1);
        assert_eq!(report.tombstones, 0);

        let got = crate::support::block_on(meta.lookup_chunk(report.commit, &coord(0, 0)))
            .unwrap()
            .unwrap();
        assert_eq!(got.blob, Some(BlobHash([9; 32])));
        let kept = crate::support::block_on(meta.lookup_chunk(report.commit, &coord(1, 0)))
            .unwrap()
            .unwrap();
        assert_eq!(kept.blob, Some(BlobHash([2; 32])));
    }

    #[test]
    fn missing_coordinates_become_tombstones() {
        let mut meta = store_with_ref();
        crate::support::block_on(meta.apply_commit_incremental(
            1_000,
            &[],
            &[
                stage_present(coord(0, 0), BlobHash([1; 32])),
                stage_present(coord(1, 0), BlobHash([2; 32])),
            ],
        ))
        .unwrap();
        let (previous, plan) =
            crate::support::block_on(plan_commit(&meta, &[], &Scope::World)).unwrap();
        assert_eq!(plan.removed, alloc::vec![key(0, 0)]);
        let staged = assemble(plan, &previous, Ingested::default(), &Scope::World);
        assert_eq!(staged.chunks, 0);
        assert_eq!(staged.tombstones, 2);

        let report =
            crate::support::block_on(record_commit(&mut meta, &previous, &staged, 2_000)).unwrap();
        assert_eq!(report.tombstones, 2);
        let tomb = crate::support::block_on(meta.lookup_chunk(report.commit, &coord(0, 0)))
            .unwrap()
            .unwrap();
        assert!(tomb.is_tombstone());
    }

    #[test]
    fn already_tombstoned_coordinates_record_no_further_rows() {
        let mut meta = store_with_ref();
        crate::support::block_on(meta.apply_commit_incremental(
            1_000,
            &[],
            &[
                stage_present(coord(0, 0), BlobHash([1; 32])),
                stage_present(coord(32, 0), BlobHash([2; 32])),
            ],
        ))
        .unwrap();

        // Region (0,0) is gone from disk: its chunk tombstones once while
        // region (1,0) is still present.
        let observed = [Observation {
            key: key(1, 0),
            fingerprint: fingerprint(key(1, 0)),
        }];
        let (previous, plan) =
            crate::support::block_on(plan_commit(&meta, &observed, &Scope::World)).unwrap();
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: alloc::vec![stage_present(coord(32, 0), BlobHash([2; 32]))],
                present: BTreeSet::from([coord(32, 0)]),
                new_blobs: 0,
            },
            &Scope::World,
        );
        assert_eq!(staged.tombstones, 1);
        assert_eq!(staged.chunks, 1);
        let report =
            crate::support::block_on(record_commit(&mut meta, &previous, &staged, 2_000)).unwrap();
        assert_eq!(report.tombstones, 1);
        assert_eq!(report.chunks, 1);

        // Repeat commit with the same world: the tombstone already resolves
        // through fallback, so it records no fresh row and the vanished
        // chunk is not carried back as present.
        let (previous, plan) =
            crate::support::block_on(plan_commit(&meta, &observed, &Scope::World)).unwrap();
        assert_eq!(
            previous.state.keys().copied().collect::<Vec<_>>(),
            [coord(32, 0)]
        );
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: alloc::vec![stage_present(coord(32, 0), BlobHash([2; 32]))],
                present: BTreeSet::from([coord(32, 0)]),
                new_blobs: 0,
            },
            &Scope::World,
        );
        assert_eq!(staged.entries, [] as [CommitEntry; 0]);
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
        crate::support::block_on(meta.apply_commit_incremental(
            1_000,
            &[],
            &[
                stage_present(coord(0, 0), BlobHash([1; 32])),
                stage_present(nether_coord(0, 0), BlobHash([2; 32])),
            ],
        ))
        .unwrap();

        // Only the nether region is observed (overworld file deleted from
        // disk), but the overworld scope sees neither ingest nor removal.
        let observed = [Observation {
            key: nether_key(0, 0),
            fingerprint: fingerprint(nether_key(0, 0)),
        }];
        let scope = Scope::dimension(NETHER);
        let (_, plan) = crate::support::block_on(plan_commit(&meta, &observed, &scope)).unwrap();
        assert_eq!(plan.ingest, alloc::vec![nether_key(0, 0)]);
        assert_eq!(plan.removed, [] as [RegionKey; 0]);

        // Out-of-scope observations never enter the plan at all.
        let observed = [
            Observation {
                key: key(0, 0),
                fingerprint: fingerprint(key(0, 0)),
            },
            Observation {
                key: nether_key(0, 0),
                fingerprint: fingerprint(nether_key(0, 0)),
            },
        ];
        let (_, plan) =
            crate::support::block_on(plan_commit(&meta, &observed, &Scope::World)).unwrap();
        assert_eq!(plan.ingest, alloc::vec![key(0, 0), nether_key(0, 0)]);
    }

    #[test]
    fn scoped_assemble_tombstones_only_in_scope() {
        let mut meta = store_with_ref();
        crate::support::block_on(meta.apply_commit_incremental(
            1_000,
            &[],
            &[
                stage_present(coord(0, 0), BlobHash([1; 32])),
                stage_present(coord(1, 0), BlobHash([2; 32])),
                stage_present(nether_coord(0, 0), BlobHash([3; 32])),
            ],
        ))
        .unwrap();

        // Scoped commit of the overworld re-ingests its changed region:
        // (1,0) is genuinely gone, the nether chunk is out of scope and
        // must not become a tombstone.
        let observed = [Observation {
            key: key(0, 0),
            fingerprint: fingerprint(key(0, 0)),
        }];
        let scope = Scope::dimension(OVER);
        let (previous, plan) =
            crate::support::block_on(plan_commit(&meta, &observed, &scope)).unwrap();
        assert_eq!(plan.ingest, alloc::vec![key(0, 0)]);
        assert_eq!(plan.removed, [] as [RegionKey; 0]);
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: alloc::vec![stage_present(coord(0, 0), BlobHash([9; 32]))],
                present: BTreeSet::from([coord(0, 0)]),
                new_blobs: 1,
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
    fn partial_scope_then_full_commit_reingests_unread_chunks() {
        let mut meta = store_with_ref();
        crate::support::block_on(meta.apply_commit_incremental(
            1_000,
            &[],
            &[
                stage_present(coord(0, 0), BlobHash([1; 32])),
                stage_present(coord(1, 0), BlobHash([2; 32])),
            ],
        ))
        .unwrap();

        // Both chunks changed on disk, but the scope reads only (0,0).
        let observed = [Observation {
            key: key(0, 0),
            fingerprint: fingerprint(key(0, 0)),
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
            },
            &scope,
        );
        assert_eq!(staged.tombstones, 0);
        crate::support::block_on(record_commit(&mut meta, &previous, &staged, 2_000)).unwrap();

        // The next full commit rereads the whole file and records the
        // unread (1,0) change, while (0,0) stays unchanged.
        let (previous, plan) =
            crate::support::block_on(plan_commit(&meta, &observed, &Scope::World)).unwrap();
        let staged = assemble(
            plan,
            &previous,
            Ingested {
                entries: alloc::vec![
                    stage_present(coord(0, 0), BlobHash([9; 32])),
                    stage_present(coord(1, 0), BlobHash([7; 32])),
                ],
                present: BTreeSet::from([coord(0, 0), coord(1, 0)]),
                new_blobs: 1,
            },
            &Scope::World,
        );
        assert_eq!(
            staged.entries,
            alloc::vec![stage_present(coord(1, 0), BlobHash([7; 32]))]
        );
        let report =
            crate::support::block_on(record_commit(&mut meta, &previous, &staged, 3_000)).unwrap();
        let got = crate::support::block_on(meta.lookup_chunk(report.commit, &coord(1, 0)))
            .unwrap()
            .unwrap();
        assert_eq!(got.blob, Some(BlobHash([7; 32])));
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
            crate::support::block_on(meta.lookup_commit(CommitId([0xFF; 32])))
                .unwrap()
                .is_none()
        );
    }
}
