//! Commit pruning: delete old commits while keeping every
//! retained commit's effective state intact.
//!
//! Deletion folds oldest-first: each retired commit moves its
//! still-effective rows onto the next retained commit and drops rows
//! already superseded there. Pruning never unlinks blobs; run GC
//! afterwards to reclaim the dereferenced ones.

#[cfg(test)]
use alloc::collections::BTreeMap;
use alloc::collections::BTreeSet;
use alloc::vec::Vec;
use core::fmt;

use crate::port::meta::MetaStore;
#[cfg(test)]
use sekai_util::{BlobHash, ChunkCoord};
use sekai_util::{Commit, CommitId};

/// Commits selected for deletion, oldest first, plus what survives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrunePlan {
    /// Commit IDs to delete, ascending.
    pub delete: Vec<CommitId>,
    /// Commit IDs to keep, ascending.
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
    /// A planned deletion has no newer retained commit to fold into;
    /// plans built by [`prune_plan`] over a non-empty retention always
    /// have one (the latest commit is retained).
    NoSuccessor {
        /// Commit without a fold target.
        id: u64,
    },
}

impl<M: fmt::Debug> fmt::Display for PruneError<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Meta(_) => write!(f, "commit metadata failed"),
            Self::NothingRetained => write!(f, "prune selection retains no commits"),
            Self::NoSuccessor { id } => {
                write!(f, "no retained successor for commit {id}")
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
    let mut ids: Vec<CommitId> = commits.iter().map(|s| s.id).collect();
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
/// Refuses selections that would leave no commit behind.
pub async fn prune_plan<M: MetaStore>(
    meta: &M,
    retained: &BTreeSet<CommitId>,
) -> Result<PrunePlan, PruneError<M::Error>> {
    let mut commits: Vec<Commit> = Vec::new();
    meta.visit_commits(|commit| {
        commits.push(commit.clone());
        true
    })
    .await
    .map_err(PruneError::Meta)?;
    if !commits.is_empty() && retained.is_empty() {
        return Err(PruneError::NothingRetained);
    }
    let mut delete: Vec<CommitId> = commits
        .iter()
        .map(|s| s.id)
        .filter(|id| !retained.contains(id))
        .collect();
    delete.sort();
    let retained: Vec<CommitId> = commits
        .iter()
        .map(|s| s.id)
        .filter(|id| retained.contains(id))
        .collect();
    Ok(PrunePlan { delete, retained })
}

/// Delete planned commits oldest-first, folding each into the next
/// retained one. `progress` fires per deleted commit as `(done, total)`.
pub async fn prune_apply<M: MetaStore>(
    meta: &mut M,
    plan: &PrunePlan,
    mut progress: impl FnMut(usize, usize) + Send,
) -> Result<PruneReport, PruneError<M::Error>> {
    let retained: BTreeSet<CommitId> = plan.retained.iter().copied().collect();
    let mut report = PruneReport {
        pruned: 0,
        rows_folded: 0,
        rows_dropped: 0,
    };
    let total = plan.delete.len();
    let mut done = 0usize;
    for id in &plan.delete {
        let Some(next) = retained.iter().find(|kept| **kept > *id).copied() else {
            return Err(PruneError::NoSuccessor { id: id.0 });
        };
        let outcome = meta
            .retire_commit(*id, next)
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
    use sekai_util::{ChunkCoord, CommitEntry, Dimension, RegionKind};

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
    fn prune_folds_oldest_first_and_keeps_effective_states() {
        use sekai_util::BlobHash;
        let mut meta = MemMeta::default();
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
