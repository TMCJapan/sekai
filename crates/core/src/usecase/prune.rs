//! Commit pruning: delete old commits while keeping every
//! retained commit's effective state intact.
//!
//! Deletion folds in ascending commit-ID order (creation order): each
//! retired commit moves its still-effective rows onto the next retained
//! commit and drops rows already superseded there. Pruning never unlinks
//! blobs; run GC afterwards to reclaim the dereferenced ones.

use crate::port::meta::MetaStore;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;
use core::fmt;
#[cfg(test)]
use sekai_util::{BlobHash, ChunkCoord};
use sekai_util::{Commit, CommitId};

/// Commits selected for deletion in ascending commit-ID order
/// (creation order), plus what survives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrunePlan {
    /// Commit IDs to delete, ascending.
    pub delete: Vec<CommitId>,
    /// Commit IDs to keep, ascending. Every ID existed at planning time;
    /// [`prune_plan`] rejects unknown IDs.
    pub retained: Vec<CommitId>,
}

/// Outcome of one [`prune_apply`] run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PruneReport {
    /// Commits deleted.
    pub pruned: usize,
    /// Rows re-stamped onto successor commits.
    pub rows_folded: usize,
    /// Rows superseded before their successor and removed.
    pub rows_dropped: usize,
}

/// Failures while planning or applying commit pruning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PruneError<M> {
    /// Metadata backend failure.
    Meta(M),
    /// The selection would delete every commit; at least the latest
    /// must survive for checkout targets to exist.
    NothingRetained,
    /// A requested retained commit does not exist.
    UnknownRetained {
        /// Requested commit ID that is not in the store.
        id: u64,
    },
    /// A planned deletion has no retained commit continuing it, so its
    /// rows cannot be folded into the retained history.
    NoSuccessor {
        /// Commit without a fold target.
        id: u64,
    },
    /// A planned deletion continues into retained commits on different
    /// branches. Folding it into one of them would strip the others of
    /// the rows they inherit through it.
    SplitBranches {
        /// Commit that cannot be retired.
        id: u64,
    },
    /// A commit is both retained and scheduled for deletion.
    OverlappingPlan {
        /// Commit present on both sides.
        id: u64,
    },
}

impl<M: fmt::Debug> fmt::Display for PruneError<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Meta(_) => write!(f, "commit metadata failed"),
            Self::NothingRetained => write!(f, "prune selection retains no commits"),
            Self::UnknownRetained { id } => {
                write!(f, "retained commit {id} does not exist")
            }
            Self::NoSuccessor { id } => {
                write!(f, "no retained commit continues commit {id}")
            }
            Self::SplitBranches { id } => write!(
                f,
                "commit {id} continues on several branches: retiring it would change their state"
            ),
            Self::OverlappingPlan { id } => {
                write!(f, "commit {id} is both retained and scheduled for deletion")
            }
        }
    }
}

/// Retain the newest `keep_last` commits intersected with `before` and
/// newer (`None` selects everything on its axis).
pub fn select_retained(
    commits: &[Commit],
    keep_last: Option<u64>,
    before: Option<CommitId>,
) -> BTreeSet<CommitId> {
    let mut ids: Vec<CommitId> = commits.iter().map(|c| c.id).collect();
    ids.sort();
    if let Some(n) = keep_last {
        let n = usize::try_from(n).unwrap_or(usize::MAX);
        let keep = ids.split_off(ids.len().saturating_sub(n));
        return keep
            .into_iter()
            .filter(|id| before.is_none_or(|b| *id >= b))
            .collect();
    }
    ids.into_iter()
        .filter(|id| before.is_none_or(|b| *id >= b))
        .collect()
}

/// List commits to delete under `retained` (everything else, ascending).
///
/// Refuses selections that would leave no commit behind, selections
/// containing an unknown retained ID, and selections that would delete a
/// commit no retained commit continues: such a commit has nowhere to fold
/// its rows into.
///
/// Every ID in `retained` must exist; the plan rechecks the store rather
/// than inventing a commit. A selection that leaves a fork point with
/// retained commits on more than one branch refuses with
/// [`PruneError::SplitBranches`]: retaining the common ancestor, or
/// pruning one branch at a time, is the caller's recourse.
pub async fn prune_plan<M: MetaStore>(
    meta: &M,
    retained: &BTreeSet<CommitId>,
) -> Result<PrunePlan, PruneError<M::Error>> {
    let commits = load_commits(meta).await?;
    if !commits.is_empty() && retained.is_empty() {
        return Err(PruneError::NothingRetained);
    }
    let existing: BTreeSet<CommitId> = commits.iter().map(|c| c.id).collect();
    if let Some(id) = retained.difference(&existing).next() {
        return Err(PruneError::UnknownRetained { id: id.0 });
    }
    let mut delete: Vec<CommitId> = commits
        .iter()
        .map(|c| c.id)
        .filter(|id| !retained.contains(id))
        .collect();
    delete.sort();
    let retained_ids: Vec<CommitId> = retained.iter().copied().collect();
    let ancestry = retained_ancestry(&commits, retained);
    for id in &delete {
        fold_target(&ancestry, retained, *id).map_err(|err| fold_failure(err, *id))?;
    }
    Ok(PrunePlan {
        delete,
        retained: retained_ids,
    })
}

/// Load the whole commit graph in one visit.
async fn load_commits<M: MetaStore>(meta: &M) -> Result<Vec<Commit>, PruneError<M::Error>> {
    let mut commits: Vec<Commit> = Vec::new();
    meta.visit_commits(|commit| {
        commits.push(commit.clone());
        true
    })
    .await
    .map_err(PruneError::Meta)?;
    Ok(commits)
}

/// Delete planned commits in ascending commit-ID order (creation order),
/// folding each into the oldest retained commit that continues it.
/// `progress` fires per deleted commit as `(done, total)`.
///
/// The fold target is a *descendant*, not merely a newer commit: folding
/// into an unrelated history would graft rows the successor never had.
/// A commit no retained commit continues cannot be retired at all.
///
/// The plan must come from [`prune_plan`]: every ID must exist, and
/// deletions and retentions must be disjoint (an overlap is refused
/// before anything is retired). Fold targets are retained, so deletion
/// order does not affect them; a missing ID fails through the fold
/// calculation or the backend's retirement checks instead of corrupting
/// the graph.
pub async fn prune_apply<M: MetaStore>(
    meta: &mut M,
    plan: &PrunePlan,
    mut progress: impl FnMut(usize, usize) + Send,
) -> Result<PruneReport, PruneError<M::Error>> {
    let retained: BTreeSet<CommitId> = plan.retained.iter().copied().collect();
    if let Some(id) = plan.delete.iter().find(|id| retained.contains(id)) {
        return Err(PruneError::OverlappingPlan { id: id.0 });
    }
    let commits = load_commits(meta).await?;
    // Retiring a commit contracts the graph: its children inherit its
    // parents, so reachability between the remaining commits does not
    // change. Every fold target is a retained commit and retained
    // commits are never retired, so one ancestry snapshot serves the
    // whole run. `retire_commit` re-checks the live descent before it
    // writes.
    let ancestry = retained_ancestry(&commits, &retained);
    let mut report = PruneReport {
        pruned: 0,
        rows_folded: 0,
        rows_dropped: 0,
    };
    let total = plan.delete.len();
    let mut done = 0usize;
    for id in &plan.delete {
        let target =
            fold_target(&ancestry, &retained, *id).map_err(|err| fold_failure(err, *id))?;
        let outcome = meta
            .retire_commit(*id, target)
            .await
            .map_err(PruneError::Meta)?;
        report.rows_folded += outcome.folded;
        report.rows_dropped += outcome.dropped;
        report.pruned += 1;
        done += 1;
        progress(done, total);
    }
    Ok(report)
}

/// The commit a retirement folds into: the oldest retained commit that
/// continues `commit` and continues every other retained commit that
/// descends from it.
///
/// Folding moves the retired commit's rows onto the target and rewires
/// its children onto its own parents. A retained commit that continues
/// the retired one through another branch would lose the rows it
/// inherits through it, so the target must be an ancestor of every
/// other retained descendant - otherwise the retirement would change
/// their effective state, and prune refuses.
///
/// `ancestry` is [`retained_ancestry`] for `retained`, computed once per
/// run: the walk costs the history depth of every retained commit.
fn fold_target(
    ancestry: &BTreeMap<CommitId, BTreeSet<CommitId>>,
    retained: &BTreeSet<CommitId>,
    commit: CommitId,
) -> Result<CommitId, FoldError> {
    let continues = |other: CommitId| {
        ancestry
            .get(&other)
            .is_some_and(|ancestors| ancestors.contains(&commit))
    };
    // The retained commits that descend from the retired one, in
    // ascending ID (creation) order: the target is the oldest of them
    // that continues all the others.
    let descendants: Vec<CommitId> = retained
        .iter()
        .copied()
        .filter(|id| continues(*id))
        .collect();
    let Some(target) = descendants.iter().copied().find(|target| {
        descendants
            .iter()
            .all(|other| other == target || ancestry.get(other).is_some_and(|a| a.contains(target)))
    }) else {
        return Err(if descendants.is_empty() {
            FoldError::NoSuccessor
        } else {
            FoldError::SplitBranches
        });
    };
    Ok(target)
}

/// Why a commit cannot be retired into the retained history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FoldError {
    /// No retained commit continues it.
    NoSuccessor,
    /// Its retained descendants live on different branches.
    SplitBranches,
}

/// The prune failure a fold error stands for.
const fn fold_failure<M>(err: FoldError, id: CommitId) -> PruneError<M> {
    match err {
        FoldError::NoSuccessor => PruneError::NoSuccessor { id: id.0 },
        FoldError::SplitBranches => PruneError::SplitBranches { id: id.0 },
    }
}

/// The ancestors of every retained commit, from the parent links.
///
/// The walk visits the ancestors of the retained commits only: the
/// retired commits are reached through them.
fn retained_ancestry(
    commits: &[Commit],
    retained: &BTreeSet<CommitId>,
) -> BTreeMap<CommitId, BTreeSet<CommitId>> {
    let parents: BTreeMap<CommitId, Vec<CommitId>> = commits
        .iter()
        .map(|commit| (commit.id, commit.parents.clone()))
        .collect();
    let mut out: BTreeMap<CommitId, BTreeSet<CommitId>> = BTreeMap::new();
    for keep in retained {
        let mut seen: BTreeSet<CommitId> = BTreeSet::new();
        let mut level: Vec<CommitId> = parents.get(keep).cloned().unwrap_or_default();
        while !level.is_empty() {
            let mut next: Vec<CommitId> = Vec::new();
            for id in level {
                if !seen.insert(id) {
                    continue;
                }
                if let Some(parents) = parents.get(&id) {
                    next.extend(parents.iter().copied());
                }
            }
            level = next;
        }
        out.insert(*keep, seen);
    }
    out
}

/// Effective present chunks per commit, for fallback-integrity checks.
#[cfg(test)]
pub(crate) fn effective_present<M: MetaStore>(
    meta: &M,
    commits: &[CommitId],
) -> Vec<BTreeMap<ChunkCoord, BlobHash>>
where
    M::Error: fmt::Debug,
{
    use crate::support::block_on;
    commits
        .iter()
        .map(|id| {
            let mut state: BTreeMap<ChunkCoord, BlobHash> = BTreeMap::new();
            block_on(meta.visit_commit_chunks(*id, |entry| {
                if let Some(blob) = entry.blob {
                    state.insert(entry.coord, blob);
                }
                true
            }))
            .unwrap();
            state
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{MemMeta, block_on};
    use sekai_util::{ChunkCoord, CommitEntry, Dimension, RefName, RegionKind};

    const OVER: Dimension = Dimension::OVERWORLD;
    const REGION: RegionKind = RegionKind::Region;

    fn coord(x: i32, z: i32) -> ChunkCoord {
        ChunkCoord::new(OVER, REGION, x, z)
    }

    /// A linear history of `n` commits, each continuing the one
    /// before it - the shape the store keeps.
    fn commits(n: u64) -> Vec<Commit> {
        (1..=n)
            .map(|id| {
                let parents = if id > 1 {
                    alloc::vec![CommitId(id - 1)]
                } else {
                    alloc::vec![]
                };
                Commit::new(CommitId(id), id * 1_000, parents)
            })
            .collect()
    }

    #[test]
    fn select_retained_intersects_axes() {
        let all = commits(5);
        let ids = |set: BTreeSet<CommitId>| set.into_iter().map(|id| id.0).collect::<Vec<_>>();
        assert_eq!(ids(select_retained(&all, None, None)), [1, 2, 3, 4, 5]);
        assert_eq!(ids(select_retained(&all, Some(2), None)), [4, 5]);
        assert_eq!(
            ids(select_retained(&all, None, Some(CommitId(3)))),
            [3, 4, 5]
        );
        assert_eq!(
            ids(select_retained(&all, Some(3), Some(CommitId(4)))),
            [4, 5]
        );
        assert!(select_retained(&all, Some(0), None).is_empty());
        assert!(select_retained(&all, None, Some(CommitId(9))).is_empty());
    }

    #[test]
    fn prune_plan_rejects_unknown_retained_ids() {
        let mut meta = MemMeta::default();
        block_on(meta.create_commit(100, &[])).unwrap();
        block_on(meta.create_commit(200, &[CommitId(1)])).unwrap();
        let retained = BTreeSet::from([CommitId(2), CommitId(99)]);
        let err = block_on(prune_plan(&meta, &retained)).unwrap_err();
        assert_eq!(err, PruneError::UnknownRetained { id: 99 });
    }

    #[test]
    fn prune_folds_oldest_first_and_keeps_effective_states() {
        use sekai_util::BlobHash;
        let mut meta = MemMeta::default();
        meta.with_active_ref(&RefName::parse("main").unwrap());
        // S1: A@0,0 B@1,0 / S2: C@0,0 (B carried) / S3: D@2,0.
        let s1 = [
            CommitEntry::new(coord(0, 0), Some(BlobHash([1; 32])), None),
            CommitEntry::new(coord(1, 0), Some(BlobHash([2; 32])), None),
        ];
        let s2 = [CommitEntry::new(coord(0, 0), Some(BlobHash([3; 32])), None)];
        let s3 = [CommitEntry::new(coord(2, 0), Some(BlobHash([4; 32])), None)];
        let mut parent: Vec<CommitId> = Vec::new();
        for (id, entries) in [
            (1u64, s1.as_slice()),
            (2, s2.as_slice()),
            (3, s3.as_slice()),
        ] {
            block_on(meta.apply_commit_incremental(id * 1_000, &parent, entries, None, &[], &[]))
                .unwrap();
            parent = alloc::vec![CommitId(id)];
        }
        let before = effective_present(&meta, &[CommitId(1), CommitId(2), CommitId(3)]);

        let retained = select_retained(&commits(3), Some(2), None);
        let plan = block_on(prune_plan(&meta, &retained)).unwrap();
        assert_eq!(plan.delete, [CommitId(1)]);
        assert_eq!(plan.retained, [CommitId(2), CommitId(3)]);
        let report = block_on(prune_apply(&mut meta, &plan, |_, _| {})).unwrap();
        assert_eq!(
            report,
            PruneReport {
                pruned: 1,
                rows_folded: 1,
                rows_dropped: 1,
            }
        );

        let after = effective_present(&meta, &[CommitId(2), CommitId(3)]);
        assert_eq!(after, before[1..]);
        assert!(block_on(meta.lookup_commit(CommitId(1))).unwrap().is_none());
    }

    #[test]
    fn prune_folds_a_chain_into_the_retained_successor() {
        use sekai_util::BlobHash;
        let mut meta = MemMeta::default();
        meta.with_active_ref(&RefName::parse("main").unwrap());
        // S1: A@0,0 / S2: B@1,0 / S3: C@2,0.
        let mut parent: Vec<CommitId> = Vec::new();
        for (id, entries) in [
            (
                1u64,
                [CommitEntry::new(coord(0, 0), Some(BlobHash([1; 32])), None)].as_slice(),
            ),
            (
                2,
                [CommitEntry::new(coord(1, 0), Some(BlobHash([2; 32])), None)].as_slice(),
            ),
            (
                3,
                [CommitEntry::new(coord(2, 0), Some(BlobHash([3; 32])), None)].as_slice(),
            ),
        ] {
            block_on(meta.apply_commit_incremental(id * 1_000, &parent, entries, None, &[], &[]))
                .unwrap();
            parent = alloc::vec![CommitId(id)];
        }
        let before = effective_present(&meta, &[CommitId(3)]);

        // Keep only the last commit: the two below it retire in one run,
        // the second folding a graph the first already changed.
        let retained = select_retained(&commits(3), Some(1), None);
        assert_eq!(retained, BTreeSet::from([CommitId(3)]));
        let plan = block_on(prune_plan(&meta, &retained)).unwrap();
        assert_eq!(plan.delete, [CommitId(1), CommitId(2)]);
        let report = block_on(prune_apply(&mut meta, &plan, |_, _| {})).unwrap();
        assert_eq!(
            report,
            PruneReport {
                pruned: 2,
                rows_folded: 2,
                rows_dropped: 0,
            }
        );

        assert!(block_on(meta.lookup_commit(CommitId(1))).unwrap().is_none());
        assert!(block_on(meta.lookup_commit(CommitId(2))).unwrap().is_none());
        assert_eq!(effective_present(&meta, &[CommitId(3)]), before);
    }

    #[test]
    fn prune_refuses_a_forking_fold() {
        use sekai_util::BlobHash;
        let mut meta = MemMeta::default();
        meta.with_active_ref(&RefName::parse("main").unwrap());
        let row = |c: ChunkCoord, byte: u8| [CommitEntry::new(c, Some(BlobHash([byte; 32])), None)];
        block_on(meta.apply_commit_incremental(1_000, &[], &row(coord(0, 0), 1), None, &[], &[]))
            .unwrap();
        // Two retained branches continue the same root: retiring it would
        // have to fold its rows into both.
        block_on(meta.apply_commit_incremental(
            2_000,
            &[CommitId(1)],
            &row(coord(1, 0), 2),
            None,
            &[],
            &[],
        ))
        .unwrap();
        // Point the active ref back at the root: the second branch is a
        // deliberate fork, which starts from commit 1 again.
        block_on(meta.set_ref(&RefName::parse("main").unwrap(), Some(CommitId(1)), 2_500)).unwrap();
        block_on(meta.apply_commit_incremental(
            3_000,
            &[CommitId(1)],
            &row(coord(2, 0), 3),
            None,
            &[],
            &[],
        ))
        .unwrap();

        let retained = BTreeSet::from([CommitId(2), CommitId(3)]);
        let err = block_on(prune_plan(&meta, &retained)).unwrap_err();
        assert!(matches!(err, PruneError::SplitBranches { id: 1 }), "{err}");
    }

    #[test]
    fn prune_folds_a_forked_parent_graph_into_one_target() {
        use sekai_util::BlobHash;
        let mut meta = MemMeta::default();
        meta.with_active_ref(&RefName::parse("main").unwrap());
        let row = |c: ChunkCoord, byte: u8| [CommitEntry::new(c, Some(BlobHash([byte; 32])), None)];
        // 1 <- 2, and 3 merges both: retiring 1 and 2 fold into 3.
        block_on(meta.apply_commit_incremental(1_000, &[], &row(coord(0, 0), 1), None, &[], &[]))
            .unwrap();
        block_on(meta.apply_commit_incremental(
            2_000,
            &[CommitId(1)],
            &row(coord(1, 0), 2),
            None,
            &[],
            &[],
        ))
        .unwrap();
        block_on(meta.apply_commit_incremental(
            3_000,
            &[CommitId(1), CommitId(2)],
            &row(coord(2, 0), 3),
            None,
            &[],
            &[],
        ))
        .unwrap();

        let before = effective_present(&meta, &[CommitId(3)]);
        let retained = BTreeSet::from([CommitId(3)]);
        let plan = block_on(prune_plan(&meta, &retained)).unwrap();
        assert_eq!(plan.delete, [CommitId(1), CommitId(2)]);
        let report = block_on(prune_apply(&mut meta, &plan, |_, _| {})).unwrap();
        assert_eq!(report.pruned, 2);
        assert_eq!(effective_present(&meta, &[CommitId(3)]), before);
        assert!(block_on(meta.lookup_commit(CommitId(1))).unwrap().is_none());
        assert!(block_on(meta.lookup_commit(CommitId(2))).unwrap().is_none());
    }

    #[test]
    fn prune_refuses_an_overlapping_plan() {
        let mut meta = MemMeta::default();
        block_on(meta.create_commit(100, &[])).unwrap();
        let plan = PrunePlan {
            delete: alloc::vec![CommitId(1)],
            retained: alloc::vec![CommitId(1)],
        };
        let err = block_on(prune_apply(&mut meta, &plan, |_, _| {})).unwrap_err();
        assert_eq!(err, PruneError::OverlappingPlan { id: 1 });
    }

    #[test]
    fn prune_refuses_empty_retention() {
        let mut meta = MemMeta::default();
        block_on(meta.create_commit(100, &[])).unwrap();
        let err = block_on(prune_plan(&meta, &BTreeSet::new())).unwrap_err();
        assert_eq!(err, PruneError::NothingRetained);
        // An empty store plans empty instead of erroring.
        let empty = MemMeta::default();
        let plan = block_on(prune_plan(&empty, &BTreeSet::new())).unwrap();
        assert_eq!(plan.delete, [] as [sekai_util::CommitId; 0]);
    }
}
