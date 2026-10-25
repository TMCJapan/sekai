//! Commit pruning: delete old commits while keeping every
//! retained commit's effective state intact.
//!
//! Deletion folds in local record order (creation order): each
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

/// Commits selected for deletion oldest-first (local sequence order),
/// plus what survives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrunePlan {
    /// Commit IDs to delete, oldest first.
    pub delete: Vec<CommitId>,
    /// Commit IDs to keep, oldest first. Every ID existed at planning time;
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
        id: CommitId,
    },
    /// A planned deletion has no retained commit continuing it, so its
    /// rows cannot be folded into the retained history.
    NoSuccessor {
        /// Commit without a fold target.
        id: CommitId,
    },
    /// A planned deletion continues into retained commits on different
    /// branches. Folding it into one of them would strip the others of
    /// the rows they inherit through it.
    SplitBranches {
        /// Commit that cannot be retired.
        id: CommitId,
    },
    /// A commit is both retained and scheduled for deletion.
    OverlappingPlan {
        /// Commit present on both sides.
        id: CommitId,
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
///
/// A `before` that names no stored commit selects nothing; callers can
/// validate unknown IDs through [`prune_plan`] first.
///
/// Ordering follows the input slice, which backends visit in local
/// sequence order; commit hashes carry no intrinsic timeline order.
pub fn select_retained(
    commits: &[Commit],
    keep_last: Option<u64>,
    before: Option<CommitId>,
) -> BTreeSet<CommitId> {
    let start = match before {
        Some(b) => match commits.iter().position(|commit| commit.id == b) {
            Some(pos) => pos,
            None => return BTreeSet::new(),
        },
        None => 0,
    };
    let candidates = &commits[start..];
    let mut ids: Vec<CommitId> = candidates.iter().map(|commit| commit.id).collect();
    if let Some(n) = keep_last {
        let n = usize::try_from(n).unwrap_or(usize::MAX);
        ids = ids.split_off(ids.len().saturating_sub(n));
    }
    ids.into_iter().collect()
}

/// List commits to delete under `retained` (everything else, oldest first).
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
        return Err(PruneError::UnknownRetained { id: *id });
    }
    let delete: Vec<CommitId> = commits
        .iter()
        .map(|commit| commit.id)
        .filter(|id| !retained.contains(id))
        .collect();
    let retained_ids: Vec<CommitId> = commits
        .iter()
        .map(|commit| commit.id)
        .filter(|id| retained.contains(id))
        .collect();
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

/// Delete planned commits oldest-first (local sequence order), folding
/// each into the oldest retained commit that continues it.
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
        return Err(PruneError::OverlappingPlan { id: *id });
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
    // The retained commits that descend from the retired one, in hash
    // order. Only one of them can continue all the others, so the
    // selection order does not pick the target.
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
        FoldError::NoSuccessor => PruneError::NoSuccessor { id },
        FoldError::SplitBranches => PruneError::SplitBranches { id },
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
    use sekai_util::{BlobHash, ChunkCoord, CommitEntry, Dimension, RefName, RegionKind};

    const OVER: Dimension = Dimension::OVERWORLD;
    const REGION: RegionKind = RegionKind::Region;

    fn coord(x: i32, z: i32) -> ChunkCoord {
        ChunkCoord::new(OVER, REGION, x, z)
    }

    fn main_ref() -> RefName {
        RefName::parse("main").unwrap()
    }

    /// A linear sequence of synthetic commits, newest last.
    fn commits(n: u8) -> Vec<Commit> {
        (1..=n)
            .map(|id| {
                let parents = if id > 1 {
                    alloc::vec![CommitId([id - 1; 32])]
                } else {
                    alloc::vec![]
                };
                Commit::new(CommitId([id; 32]), u64::from(id) * 1_000, parents)
            })
            .collect()
    }

    #[test]
    fn select_retained_intersects_axes() {
        let all = commits(5);
        let retained = |keep_last, before| {
            select_retained(&all, keep_last, before)
                .into_iter()
                .collect::<Vec<_>>()
        };
        assert_eq!(
            retained(None, None),
            (1..=5).map(|id| CommitId([id; 32])).collect::<Vec<_>>()
        );
        assert_eq!(
            retained(Some(2), None),
            [CommitId([4; 32]), CommitId([5; 32])]
        );
        assert_eq!(
            retained(None, Some(CommitId([3; 32]))),
            [CommitId([3; 32]), CommitId([4; 32]), CommitId([5; 32])]
        );
        assert_eq!(
            retained(Some(3), Some(CommitId([4; 32]))),
            [CommitId([4; 32]), CommitId([5; 32])]
        );
        assert_eq!(retained(Some(0), None), [] as [CommitId; 0]);
        assert_eq!(retained(None, Some(CommitId([9; 32]))), [] as [CommitId; 0]);
    }

    #[test]
    fn prune_plan_rejects_unknown_retained_ids() {
        let mut meta = MemMeta::default();
        let first = block_on(meta.create_commit(100, &[])).unwrap();
        let second = block_on(meta.create_commit(200, &[first])).unwrap();
        let missing = CommitId([0xFF; 32]);
        let retained = BTreeSet::from([second, missing]);
        let err = block_on(prune_plan(&meta, &retained)).unwrap_err();
        assert_eq!(err, PruneError::UnknownRetained { id: missing });
    }

    #[test]
    fn prune_folds_oldest_first_and_keeps_effective_states() {
        let mut meta = MemMeta::default();
        meta.with_active_ref(&main_ref());
        // S1: A@0,0 B@1,0 / S2: C@0,0 (B carried) / S3: D@2,0.
        let s1 = [
            CommitEntry::new(coord(0, 0), Some(BlobHash([1; 32])), None),
            CommitEntry::new(coord(1, 0), Some(BlobHash([2; 32])), None),
        ];
        let s2 = [CommitEntry::new(coord(0, 0), Some(BlobHash([3; 32])), None)];
        let s3 = [CommitEntry::new(coord(2, 0), Some(BlobHash([4; 32])), None)];
        let first = block_on(meta.apply_commit_incremental(1_000, &[], &s1, None, &[], &[]))
            .unwrap()
            .id;
        let second = block_on(meta.apply_commit_incremental(2_000, &[first], &s2, None, &[], &[]))
            .unwrap()
            .id;
        let third = block_on(meta.apply_commit_incremental(3_000, &[second], &s3, None, &[], &[]))
            .unwrap()
            .id;
        let before = effective_present(&meta, &[first, second, third]);

        let retained = BTreeSet::from([second, third]);
        let plan = block_on(prune_plan(&meta, &retained)).unwrap();
        assert_eq!(plan.delete, [first]);
        assert_eq!(plan.retained, [second, third]);
        let report = block_on(prune_apply(&mut meta, &plan, |_, _| {})).unwrap();
        assert_eq!(
            report,
            PruneReport {
                pruned: 1,
                rows_folded: 1,
                rows_dropped: 1,
            }
        );

        let after = effective_present(&meta, &[second, third]);
        assert_eq!(after, before[1..]);
        assert!(block_on(meta.lookup_commit(first)).unwrap().is_none());
    }

    #[test]
    fn prune_folds_a_chain_into_the_retained_successor() {
        let mut meta = MemMeta::default();
        meta.with_active_ref(&main_ref());
        // S1: A@0,0 / S2: B@1,0 / S3: C@2,0.
        let rows = |x: i32, byte: u8| {
            [CommitEntry::new(
                coord(x, 0),
                Some(BlobHash([byte; 32])),
                None,
            )]
        };
        let first =
            block_on(meta.apply_commit_incremental(1_000, &[], &rows(0, 1), None, &[], &[]))
                .unwrap()
                .id;
        let second =
            block_on(meta.apply_commit_incremental(2_000, &[first], &rows(1, 2), None, &[], &[]))
                .unwrap()
                .id;
        let third =
            block_on(meta.apply_commit_incremental(3_000, &[second], &rows(2, 3), None, &[], &[]))
                .unwrap()
                .id;
        let before = effective_present(&meta, &[third]);

        // Keep only the last commit: the two below it retire in one run,
        // the second folding a graph the first already changed.
        let retained = BTreeSet::from([third]);
        let plan = block_on(prune_plan(&meta, &retained)).unwrap();
        assert_eq!(plan.delete, [first, second]);
        let report = block_on(prune_apply(&mut meta, &plan, |_, _| {})).unwrap();
        assert_eq!(
            report,
            PruneReport {
                pruned: 2,
                rows_folded: 2,
                rows_dropped: 0,
            }
        );

        assert!(block_on(meta.lookup_commit(first)).unwrap().is_none());
        assert!(block_on(meta.lookup_commit(second)).unwrap().is_none());
        assert_eq!(effective_present(&meta, &[third]), before);
    }

    #[test]
    fn prune_refuses_a_forking_fold() {
        let mut meta = MemMeta::default();
        meta.with_active_ref(&main_ref());
        let row = |c: ChunkCoord, byte: u8| [CommitEntry::new(c, Some(BlobHash([byte; 32])), None)];
        let first = block_on(meta.apply_commit_incremental(
            1_000,
            &[],
            &row(coord(0, 0), 1),
            None,
            &[],
            &[],
        ))
        .unwrap()
        .id;
        // Two retained branches continue the same root: retiring it would
        // have to fold its rows into both.
        let second = block_on(meta.apply_commit_incremental(
            2_000,
            &[first],
            &row(coord(1, 0), 2),
            None,
            &[],
            &[],
        ))
        .unwrap()
        .id;
        // Point the active ref back at the root: the third commit is a
        // deliberate fork.
        block_on(meta.set_ref(&main_ref(), Some(first), 2_500)).unwrap();
        let third = block_on(meta.apply_commit_incremental(
            3_000,
            &[first],
            &row(coord(2, 0), 3),
            None,
            &[],
            &[],
        ))
        .unwrap()
        .id;

        let retained = BTreeSet::from([second, third]);
        let err = block_on(prune_plan(&meta, &retained)).unwrap_err();
        assert!(
            matches!(err, PruneError::SplitBranches { id } if id == first),
            "{err}"
        );
    }

    #[test]
    fn prune_folds_a_forked_parent_graph_into_one_target() {
        let mut meta = MemMeta::default();
        meta.with_active_ref(&main_ref());
        let row = |c: ChunkCoord, byte: u8| [CommitEntry::new(c, Some(BlobHash([byte; 32])), None)];
        // first <- second, and third merges both: retiring first and
        // second fold into third.
        let first = block_on(meta.apply_commit_incremental(
            1_000,
            &[],
            &row(coord(0, 0), 1),
            None,
            &[],
            &[],
        ))
        .unwrap()
        .id;
        let second = block_on(meta.apply_commit_incremental(
            2_000,
            &[first],
            &row(coord(1, 0), 2),
            None,
            &[],
            &[],
        ))
        .unwrap()
        .id;
        let third = block_on(meta.apply_commit_incremental(
            3_000,
            &[first, second],
            &row(coord(2, 0), 3),
            None,
            &[],
            &[],
        ))
        .unwrap()
        .id;

        let before = effective_present(&meta, &[third]);
        let retained = BTreeSet::from([third]);
        let plan = block_on(prune_plan(&meta, &retained)).unwrap();
        assert_eq!(plan.delete, [first, second]);
        let report = block_on(prune_apply(&mut meta, &plan, |_, _| {})).unwrap();
        assert_eq!(report.pruned, 2);
        assert_eq!(effective_present(&meta, &[third]), before);
        assert!(block_on(meta.lookup_commit(first)).unwrap().is_none());
        assert!(block_on(meta.lookup_commit(second)).unwrap().is_none());
    }

    #[test]
    fn prune_refuses_an_overlapping_plan() {
        let mut meta = MemMeta::default();
        let first = block_on(meta.create_commit(100, &[])).unwrap();
        let plan = PrunePlan {
            delete: alloc::vec![first],
            retained: alloc::vec![first],
        };
        let err = block_on(prune_apply(&mut meta, &plan, |_, _| {})).unwrap_err();
        assert_eq!(err, PruneError::OverlappingPlan { id: first });
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
        assert_eq!(plan.delete, [] as [CommitId; 0]);
    }
}
