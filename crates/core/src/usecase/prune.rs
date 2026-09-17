//! Snapshot pruning: delete old snapshots while keeping every
//! retained snapshot's effective state intact.
//!
//! Deletion folds oldest-first: each retired snapshot moves its
//! still-effective rows onto the next retained snapshot and drops rows
//! already superseded there. Pruning never unlinks blobs; run GC
//! afterwards to reclaim the dereferenced ones.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;
use core::fmt;

use crate::port::meta::MetaStore;
use sekai_util::{Snapshot, SnapshotId};

/// Snapshots selected for deletion, oldest first, plus what survives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrunePlan {
    /// Snapshot IDs to delete, ascending.
    pub delete: Vec<SnapshotId>,
    /// Snapshot IDs to keep, ascending.
    pub retained: Vec<SnapshotId>,
}

/// Outcome of one [`prune_apply`] run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PruneReport {
    /// Snapshots deleted.
    pub pruned: usize,
    /// Rows re-stamped onto successor snapshots.
    pub rows_folded: usize,
    /// Rows superseded before their successor and removed.
    pub rows_dropped: usize,
}

/// Failures while planning or applying snapshot pruning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PruneError<M> {
    /// Metadata backend failure.
    Meta(M),
    /// The selection would delete every snapshot; at least the latest
    /// must survive for rollback targets to exist.
    NothingRetained,
    /// A planned deletion has no newer retained snapshot to fold into;
    /// plans built by [`prune_plan`] over a non-empty retention always
    /// have one (the latest snapshot is retained).
    NoSuccessor {
        /// Snapshot without a fold target.
        id: u64,
    },
}

impl<M: fmt::Debug> fmt::Display for PruneError<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Meta(_) => write!(f, "snapshot metadata failed"),
            Self::NothingRetained => write!(f, "prune selection retains no snapshots"),
            Self::NoSuccessor { id } => {
                write!(f, "no retained successor for snapshot {id}")
            }
        }
    }
}

/// Retain the newest `keep_last` snapshots intersected with `before` and
/// newer (`None` selects everything on its axis).
pub fn select_retained(
    snapshots: &[Snapshot],
    keep_last: Option<u64>,
    before: Option<SnapshotId>,
) -> BTreeSet<SnapshotId> {
    let mut ids: Vec<SnapshotId> = snapshots.iter().map(|s| s.id).collect();
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

/// List snapshots to delete under `retained` (everything else, ascending).
/// Refuses selections that would leave no snapshot behind.
pub async fn prune_plan<M: MetaStore>(
    meta: &M,
    retained: &BTreeSet<SnapshotId>,
) -> Result<PrunePlan, PruneError<M::Error>> {
    let mut snapshots: Vec<Snapshot> = Vec::new();
    meta.visit_snapshots(|snapshot| {
        snapshots.push(*snapshot);
        true
    })
    .await
    .map_err(PruneError::Meta)?;
    if !snapshots.is_empty() && retained.is_empty() {
        return Err(PruneError::NothingRetained);
    }
    let mut delete: Vec<SnapshotId> = snapshots
        .iter()
        .map(|s| s.id)
        .filter(|id| !retained.contains(id))
        .collect();
    delete.sort();
    let retained: Vec<SnapshotId> = snapshots
        .iter()
        .map(|s| s.id)
        .filter(|id| retained.contains(id))
        .collect();
    Ok(PrunePlan { delete, retained })
}

/// Delete planned snapshots oldest-first, folding each into the next
/// retained one. `progress` fires per deleted snapshot as `(done, total)`.
pub async fn prune_apply<M: MetaStore>(
    meta: &mut M,
    plan: &PrunePlan,
    mut progress: impl FnMut(usize, usize) + Send,
) -> Result<PruneReport, PruneError<M::Error>> {
    let retained: BTreeSet<SnapshotId> = plan.retained.iter().copied().collect();
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
            .retire_snapshot(*id, next)
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

/// Effective present chunks per snapshot, for fallback-integrity checks.
#[cfg(test)]
pub(crate) fn effective_present<M: MetaStore>(
    meta: &M,
    snapshots: &[SnapshotId],
) -> Vec<alloc::collections::BTreeMap<sekai_util::ChunkCoord, sekai_util::BlobHash>>
where
    M::Error: fmt::Debug,
{
    use crate::support::block_on;
    snapshots
        .iter()
        .map(|id| {
            let mut state: alloc::collections::BTreeMap<
                sekai_util::ChunkCoord,
                sekai_util::BlobHash,
            > = alloc::collections::BTreeMap::new();
            block_on(meta.visit_snapshot_chunks(*id, |entry| {
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
    use sekai_util::{ChunkCoord, Dimension, RegionKind, SnapshotEntry};

    const OVER: Dimension = Dimension::OVERWORLD;
    const REGION: RegionKind = RegionKind::REGION;

    fn coord(x: i32, z: i32) -> ChunkCoord {
        ChunkCoord::new(OVER, REGION, x, z)
    }

    fn snapshots(n: u64) -> Vec<Snapshot> {
        (1..=n)
            .map(|id| Snapshot::new(SnapshotId(id), id * 1_000))
            .collect()
    }

    #[test]
    fn select_retained_intersects_axes() {
        let all = snapshots(5);
        let ids = |set: BTreeSet<SnapshotId>| set.into_iter().map(|id| id.0).collect::<Vec<_>>();
        assert_eq!(ids(select_retained(&all, None, None)), [1, 2, 3, 4, 5]);
        assert_eq!(ids(select_retained(&all, Some(2), None)), [4, 5]);
        assert_eq!(
            ids(select_retained(&all, None, Some(SnapshotId(3)))),
            [3, 4, 5]
        );
        assert_eq!(
            ids(select_retained(&all, Some(3), Some(SnapshotId(4)))),
            [4, 5]
        );
        assert!(select_retained(&all, Some(0), None).is_empty());
        assert!(select_retained(&all, None, Some(SnapshotId(9))).is_empty());
    }

    #[test]
    fn prune_folds_oldest_first_and_keeps_effective_states() {
        use sekai_util::BlobHash;
        let mut meta = MemMeta::default();
        // S1: A@0,0 B@1,0 / S2: C@0,0 (B carried) / S3: D@2,0.
        let s1 = [
            SnapshotEntry::new(coord(0, 0), Some(BlobHash([1; 32])), None),
            SnapshotEntry::new(coord(1, 0), Some(BlobHash([2; 32])), None),
        ];
        let s2 = [SnapshotEntry::new(
            coord(0, 0),
            Some(BlobHash([3; 32])),
            None,
        )];
        let s3 = [SnapshotEntry::new(
            coord(2, 0),
            Some(BlobHash([4; 32])),
            None,
        )];
        for (id, entries) in [
            (1u64, s1.as_slice()),
            (2, s2.as_slice()),
            (3, s3.as_slice()),
        ] {
            block_on(meta.apply_snapshot_incremental(id * 1_000, entries, None, &[], &[])).unwrap();
        }
        let before = effective_present(&meta, &[SnapshotId(1), SnapshotId(2), SnapshotId(3)]);

        let retained = select_retained(&snapshots(3), Some(2), None);
        let plan = block_on(prune_plan(&meta, &retained)).unwrap();
        assert_eq!(plan.delete, [SnapshotId(1)]);
        assert_eq!(plan.retained, [SnapshotId(2), SnapshotId(3)]);
        let report = block_on(prune_apply(&mut meta, &plan, |_, _| {})).unwrap();
        assert_eq!(
            report,
            PruneReport {
                pruned: 1,
                rows_folded: 1,
                rows_dropped: 1,
            }
        );

        let after = effective_present(&meta, &[SnapshotId(2), SnapshotId(3)]);
        assert_eq!(after, before[1..]);
        assert!(
            block_on(meta.lookup_snapshot(SnapshotId(1)))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn prune_refuses_empty_retention() {
        let mut meta = MemMeta::default();
        block_on(meta.create_snapshot(100)).unwrap();
        let err = block_on(prune_plan(&meta, &BTreeSet::new())).unwrap_err();
        assert_eq!(err, PruneError::NothingRetained);
        // An empty store plans empty instead of erroring.
        let empty = MemMeta::default();
        let plan = block_on(prune_plan(&empty, &BTreeSet::new())).unwrap();
        assert!(plan.delete.is_empty());
    }
}
