//! Commit-graph read operations: listing, parents, ancestors, history,
//! and reachability.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;
use core::fmt;

use sekai_storage::MetaStore;
use sekai_util::{Commit, CommitId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitError<M> {
    /// Metadata backend failure.
    Meta(M),
    /// No commit with this ID exists.
    UnknownCommit {
        /// Requested commit ID.
        id: CommitId,
    },
}

impl<M: fmt::Debug> fmt::Display for CommitError<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Meta(_) => write!(f, "commit metadata failed"),
            Self::UnknownCommit { id } => write!(f, "unknown commit: {id}"),
        }
    }
}

/// The commits a commit continues: its direct parents.
///
/// Fails when the commit does not exist: an unknown ID must not read
/// as the root of a history.
pub async fn commit_parents<M: MetaStore>(
    meta: &M,
    commit: CommitId,
) -> Result<Vec<CommitId>, CommitError<M::Error>> {
    // The record carries the parents, so the commit is read once.
    let record = lookup_record(meta, commit).await?;
    Ok(record.parents)
}

/// Breadth-first walk over parent links from `seeds`. Within one
/// generation the commits come in ascending hash order; `visit` runs
/// for every new commit, nearest first, and may return true to stop
/// the walk early. The seeds are visited too.
///
/// A commit continues commits that are older than it, so the graph is
/// acyclic by construction; the visited set is the guard that keeps
/// the walk finite even if that ever changes.
async fn walk_history<M: MetaStore>(
    meta: &M,
    seeds: impl IntoIterator<Item = CommitId>,
    mut visit: impl FnMut(Commit) -> bool,
) -> Result<(), CommitError<M::Error>> {
    let mut seen: BTreeSet<CommitId> = BTreeSet::new();
    let mut level: BTreeSet<CommitId> = seeds.into_iter().collect();
    while !level.is_empty() {
        let mut next: BTreeSet<CommitId> = BTreeSet::new();
        for id in level {
            if !seen.insert(id) {
                continue;
            }
            let record = lookup_record(meta, id).await?;
            for parent in &record.parents {
                if !seen.contains(parent) {
                    next.insert(*parent);
                }
            }
            if visit(record) {
                return Ok(());
            }
        }
        level = next;
    }
    Ok(())
}

/// Depth-first walk over parent links from `pending` into `seen`,
/// visiting each commit once. When `stop_at` is set, the walk stops at
/// that commit and reports `true`; otherwise it drains `pending` and
/// reports `false`. Commits already in `seen` are skipped.
///
/// One record is loaded per visited commit: a missing record fails the
/// walk loudly rather than reading as a root of history.
async fn walk_parents<M: MetaStore>(
    meta: &M,
    seen: &mut BTreeSet<CommitId>,
    pending: &mut Vec<CommitId>,
    stop_at: Option<CommitId>,
) -> Result<bool, CommitError<M::Error>> {
    while let Some(id) = pending.pop() {
        if stop_at == Some(id) {
            return Ok(true);
        }
        if seen.insert(id) {
            pending.extend(lookup_record(meta, id).await?.parents);
        }
    }
    Ok(false)
}

/// Every ancestor of `commit`, nearest first.
///
/// The walk follows parent links, so it covers the whole history the
/// commit descends from. The ancestors come by distance: the direct
/// parents first, then their parents, and so on; within one distance
/// they come in ascending hash order. Each commit is reported once.
///
/// A commit continues commits that are older than it, so the graph
/// is acyclic by construction; the visited set is the guard that keeps
/// the walk finite even if that ever changes.
pub async fn commit_ancestors<M: MetaStore>(
    meta: &M,
    commit: CommitId,
) -> Result<Vec<CommitId>, CommitError<M::Error>> {
    // The record carries the parents, so the requested commit is read
    // once and the walk starts from its parents.
    let record = lookup_record(meta, commit).await?;
    let mut order = Vec::new();
    walk_history(meta, record.parents, |record| {
        order.push(record.id);
        false
    })
    .await?;
    Ok(order)
}

/// The history of `commit`: the commit itself followed by its
/// ancestors, nearest first.
///
/// One walk reads the whole history: a record carries the parents the
/// walk follows. Every ancestor must have a record - a missing one is a
/// broken history, not a shorter history.
pub async fn commit_history<M: MetaStore>(
    meta: &M,
    commit: CommitId,
) -> Result<Vec<Commit>, CommitError<M::Error>> {
    let mut out = Vec::new();
    walk_history(meta, [commit], |record| {
        out.push(record);
        false
    })
    .await?;
    Ok(out)
}

/// Whether `ancestor` is `descendant` itself or one of its ancestors, as
/// `git merge-base --is-ancestor` asks. `branch -d` asks it of a branch
/// and HEAD: a branch HEAD reaches is merged.
///
/// An unknown `descendant` is an error. An unknown `ancestor` lies in no
/// history, so the answer is `false`. As in [`commit_ancestors`], the
/// record of every commit the walk visits must exist: a missing one is a
/// broken history, not a shorter one.
pub async fn is_ancestor<M: MetaStore>(
    meta: &M,
    ancestor: CommitId,
    descendant: CommitId,
) -> Result<bool, CommitError<M::Error>> {
    let record = lookup_record(meta, descendant).await?;
    if ancestor == descendant {
        return Ok(true);
    }
    let mut seen = BTreeSet::from([descendant]);
    let mut pending = record.parents;
    walk_parents(meta, &mut seen, &mut pending, Some(ancestor)).await
}

/// Every commit reachable from `starts` by parent links, `starts` included.
///
/// An unknown starting point is an error. As in [`is_ancestor`], the
/// record of every commit the walk visits must exist: a missing one is a
/// broken history, not a shorter one.
pub(super) async fn reachable_from<M: MetaStore>(
    meta: &M,
    starts: &[CommitId],
) -> Result<BTreeSet<CommitId>, CommitError<M::Error>> {
    let mut seen = BTreeSet::new();
    let mut pending = Vec::with_capacity(starts.len());
    for &start in starts {
        if seen.insert(start) {
            pending.extend(lookup_record(meta, start).await?.parents);
        }
    }
    walk_parents(meta, &mut seen, &mut pending, None).await?;
    Ok(seen)
}

/// Read one commit record; a missing record is a broken history.
pub(super) async fn lookup_record<M: MetaStore>(
    meta: &M,
    commit: CommitId,
) -> Result<Commit, CommitError<M::Error>> {
    meta.lookup_commit(commit)
        .await
        .map_err(CommitError::Meta)?
        .ok_or(CommitError::UnknownCommit { id: commit })
}

/// Per-commit change statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitStats {
    /// Present chunks whose blob differs from the first parent.
    pub fresh_chunks: usize,
    /// Coordinates present at the parent but absent from this commit.
    pub fresh_tombstones: usize,
    /// Distinct new blobs among the fresh chunks.
    pub new_blobs: usize,
    /// Present chunks in the commit's snapshot.
    pub effective_chunks: usize,
}

/// Count fresh and effective chunks for one commit.
///
/// Fresh chunks are the tree diff against the first parent (the base the
/// tree merge would have used); a root commit diffs against the empty
/// snapshot. Subtrees whose hashes match are skipped, not materialized.
pub async fn commit_stats<M: MetaStore>(
    meta: &M,
    commit: CommitId,
) -> Result<CommitStats, CommitError<M::Error>> {
    let record = lookup_record(meta, commit).await?;
    let base = match record.parents.first().copied() {
        Some(parent) => Some(lookup_record(meta, parent).await?.root),
        None => None,
    };
    let stats = crate::tree::diff_stats(meta, record.root, base)
        .await
        .map_err(CommitError::Meta)?;
    Ok(CommitStats {
        fresh_chunks: stats.fresh_chunks,
        fresh_tombstones: stats.fresh_tombstones,
        new_blobs: stats.new_blobs,
        effective_chunks: stats.effective_chunks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{MemMeta, block_on, fork, main_worktree};
    use sekai_util::{CommitEntry, Head, RefName, ReflogNote};

    #[test]
    fn counts_fresh_and_effective_rows() {
        use sekai_util::{ChunkCoord, ChunkHash, Dimension, RegionKind};
        let mut meta = MemMeta::default();
        meta.with_main();
        let over = Dimension::OVERWORLD;
        let region = RegionKind::Region;
        let first = block_on(meta.apply_main(
            100,
            &[],
            &[
                CommitEntry::new(
                    ChunkCoord::new(over, region, 0, 0),
                    Some(ChunkHash::new([1; 32])),
                ),
                CommitEntry::new(
                    ChunkCoord::new(over, region, 1, 0),
                    Some(ChunkHash::new([1; 32])),
                ),
            ],
        ))
        .unwrap()
        .id;
        let second = block_on(meta.apply_main(
            200,
            &[first],
            &[
                CommitEntry::new(
                    ChunkCoord::new(over, region, 0, 0),
                    Some(ChunkHash::new([2; 32])),
                ),
                CommitEntry::new(ChunkCoord::new(over, region, 1, 0), None),
            ],
        ))
        .unwrap()
        .id;

        assert_eq!(
            block_on(commit_stats(&meta, first)).unwrap(),
            CommitStats {
                fresh_chunks: 2,
                fresh_tombstones: 0,
                new_blobs: 1,
                effective_chunks: 2,
            }
        );
        assert_eq!(
            block_on(commit_stats(&meta, second)).unwrap(),
            CommitStats {
                fresh_chunks: 1,
                fresh_tombstones: 1,
                new_blobs: 1,
                effective_chunks: 1,
            }
        );
    }

    #[test]
    fn counts_vanished_regions_and_dimensions() {
        use sekai_util::{ChunkCoord, ChunkHash, Dimension, RegionKind};
        let mut meta = MemMeta::default();
        meta.with_main();
        let over = Dimension::OVERWORLD;
        let nether = Dimension::NETHER;
        let region = RegionKind::Region;
        let entry = |dim, x, blob: Option<[u8; 32]>| {
            CommitEntry::new(ChunkCoord::new(dim, region, x, 0), blob.map(ChunkHash::new))
        };
        // Two overworld regions share one node by content, and the nether
        // starts its own dimension.
        let first = block_on(meta.apply_main(
            100,
            &[],
            &[
                entry(over, 0, Some([1; 32])),
                entry(over, 1, Some([2; 32])),
                entry(over, 32, Some([3; 32])),
                entry(over, 64, Some([1; 32])),
                entry(over, 65, Some([2; 32])),
                entry(nether, 0, Some([4; 32])),
            ],
        ))
        .unwrap()
        .id;
        // Change one region, drop another whole; the shared node and the
        // nether stay untouched.
        let second = block_on(meta.apply_main(
            200,
            &[first],
            &[entry(over, 0, Some([5; 32])), entry(over, 32, None)],
        ))
        .unwrap()
        .id;
        // Drop the nether's last chunk: the whole dimension vanishes.
        let third = block_on(meta.apply_main(300, &[second], &[entry(nether, 0, None)]))
            .unwrap()
            .id;

        assert_eq!(
            block_on(commit_stats(&meta, first)).unwrap(),
            CommitStats {
                fresh_chunks: 6,
                fresh_tombstones: 0,
                new_blobs: 4,
                effective_chunks: 6,
            }
        );
        assert_eq!(
            block_on(commit_stats(&meta, second)).unwrap(),
            CommitStats {
                fresh_chunks: 1,
                fresh_tombstones: 1,
                new_blobs: 1,
                effective_chunks: 5,
            }
        );
        assert_eq!(
            block_on(commit_stats(&meta, third)).unwrap(),
            CommitStats {
                fresh_chunks: 0,
                fresh_tombstones: 1,
                new_blobs: 0,
                effective_chunks: 4,
            }
        );
    }

    #[test]
    fn walks_the_history_back_to_the_root() {
        let mut meta = MemMeta::default();
        let root = meta.create_commit(100, &[]).unwrap();
        let second = meta.create_commit(200, &[root]).unwrap();
        let third = meta.create_commit(300, &[second]).unwrap();

        assert_eq!(
            block_on(commit_ancestors(&meta, third)).unwrap(),
            [second, root]
        );
        assert_eq!(
            block_on(commit_ancestors(&meta, root)).unwrap(),
            [] as [CommitId; 0]
        );

        let walked = block_on(commit_history(&meta, third)).unwrap();
        assert_eq!(
            walked.iter().map(|c| c.id).collect::<Vec<_>>(),
            [third, second, root]
        );
        assert_eq!(walked[0].parents, [second]);
        assert!(walked[2].is_root());
    }

    #[test]
    fn a_commit_only_inherits_from_its_own_ancestors() {
        use sekai_util::{ChunkCoord, ChunkHash, Dimension, RegionKind};
        let mut meta = MemMeta::default();
        meta.with_main();
        let coord = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
        let first_only = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Region, 1, 0);
        let first = block_on(meta.apply_main(
            100,
            &[],
            &[
                CommitEntry::new(coord, Some(ChunkHash::new([1; 32]))),
                CommitEntry::new(first_only, Some(ChunkHash::new([3; 32]))),
            ],
        ))
        .unwrap()
        .id;
        // A second history: its root shares no ancestor with `first`.
        let orphan = Head::Branch(RefName::parse("orphan").unwrap());
        block_on(meta.set_head(&main_worktree(), &orphan, ReflogNote::new("checkout", 150)))
            .unwrap();
        let other = block_on(meta.apply_main(
            200,
            &[],
            &[CommitEntry::new(coord, Some(ChunkHash::new([2; 32])))],
        ))
        .unwrap()
        .id;
        let child = block_on(meta.apply_main(300, &[other], &[])).unwrap().id;

        // The child continues `other`, which holds a different blob.
        assert_eq!(
            block_on(meta.lookup_chunk(child, &coord))
                .unwrap()
                .map(|entry| entry.blob),
            Some(Some(ChunkHash::new([2; 32])))
        );
        // A sibling history's chunk never leaks into the child, even one
        // the child's own history never recorded.
        assert!(
            block_on(meta.lookup_chunk(child, &first_only))
                .unwrap()
                .is_none()
        );
        assert!(
            block_on(meta.lookup_chunk(first, &first_only))
                .unwrap()
                .is_some()
        );
        assert_eq!(
            block_on(meta.lookup_chunk(first, &coord))
                .unwrap()
                .map(|entry| entry.blob),
            Some(Some(ChunkHash::new([1; 32])))
        );
    }

    #[test]
    fn is_ancestor_follows_parents_only() {
        let (meta, [a, b, c]) = fork();
        assert_eq!(block_on(is_ancestor(&meta, a, b)), Ok(true));
        assert_eq!(block_on(is_ancestor(&meta, b, b)), Ok(true));
        assert_eq!(block_on(is_ancestor(&meta, b, a)), Ok(false));
        assert_eq!(block_on(is_ancestor(&meta, c, b)), Ok(false));
        let missing = CommitId([9; 32]);
        assert_eq!(
            block_on(is_ancestor(&meta, a, missing)),
            Err(CommitError::UnknownCommit { id: missing })
        );
        assert_eq!(
            block_on(is_ancestor(&meta, missing, missing)),
            Err(CommitError::UnknownCommit { id: missing })
        );
        assert_eq!(block_on(is_ancestor(&meta, missing, b)), Ok(false));
    }

    #[test]
    fn reachable_from_checks_its_starting_points() {
        let (meta, [a, b, c]) = fork();
        assert_eq!(
            block_on(reachable_from(&meta, &[b, c])),
            Ok(BTreeSet::from([a, b, c]))
        );
        assert_eq!(block_on(reachable_from(&meta, &[])), Ok(BTreeSet::new()));
        let missing = CommitId([9; 32]);
        assert_eq!(
            block_on(reachable_from(&meta, &[b, missing])),
            Err(CommitError::UnknownCommit { id: missing })
        );
    }

    #[test]
    fn an_unknown_commit_is_not_a_root() {
        let meta = MemMeta::default();
        let id = CommitId([7; 32]);
        assert_eq!(
            block_on(commit_parents(&meta, id)).unwrap_err(),
            CommitError::UnknownCommit { id }
        );
        assert_eq!(
            block_on(commit_ancestors(&meta, id)).unwrap_err(),
            CommitError::UnknownCommit { id }
        );
        assert_eq!(
            block_on(commit_history(&meta, id)).unwrap_err(),
            CommitError::UnknownCommit { id }
        );
    }

    #[test]
    fn a_commit_cannot_name_a_missing_parent() {
        use crate::support::MemError;
        let mut meta = MemMeta::default();
        let missing = CommitId([9; 32]);
        assert_eq!(
            meta.create_commit(100, &[missing]).unwrap_err(),
            MemError::UnknownParent { id: missing }
        );
        // A parent listed twice is recorded once.
        let root = meta.create_commit(100, &[]).unwrap();
        let child = meta.create_commit(200, &[root, root]).unwrap();
        assert_eq!(block_on(meta.commit_parents(child)).unwrap(), [root]);
    }

    #[test]
    fn repeating_a_commit_is_idempotent() {
        let mut meta = MemMeta::default();
        let first = meta.create_commit(100, &[]).unwrap();
        let again = meta.create_commit(100, &[]).unwrap();
        assert_eq!(first, again);
    }
}
