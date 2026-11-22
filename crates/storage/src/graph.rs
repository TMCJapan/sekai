//! Commit-graph read operations: listing, parents, ancestors, history, and
//! reference resolution.
//!
//! Generic over [`MetaStore`], like the tree walks: backends and callers
//! share one implementation of each graph read.

use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use sekai_util::{Commit, CommitId, Ref, RefName, Revision, RevisionBase};

use crate::meta::MetaStore;

/// Failures while reading commit metadata.
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

/// List all commits in local record order (oldest first).
pub async fn list_commits<M: MetaStore>(meta: &M) -> Result<Vec<Commit>, CommitError<M::Error>> {
    let mut out = Vec::new();
    meta.visit_commits(|commit| {
        out.push(commit.clone());
        true
    })
    .await
    .map_err(CommitError::Meta)?;
    Ok(out)
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
    let mut seen: BTreeSet<CommitId> = BTreeSet::new();
    let mut order: Vec<CommitId> = Vec::new();
    let mut level: BTreeSet<CommitId> = record.parents.into_iter().collect();
    while !level.is_empty() {
        let mut next: BTreeSet<CommitId> = BTreeSet::new();
        for id in level {
            if !seen.insert(id) {
                continue;
            }
            order.push(id);
            for parent in meta.commit_parents(id).await.map_err(CommitError::Meta)? {
                if !seen.contains(&parent) {
                    next.insert(parent);
                }
            }
        }
        level = next;
    }
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
    let mut seen: BTreeSet<CommitId> = BTreeSet::new();
    let mut level: BTreeSet<CommitId> = BTreeSet::from([commit]);
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
            out.push(record);
        }
        level = next;
    }
    Ok(out)
}

/// Read one commit record; a missing record is a broken history.
async fn lookup_record<M: MetaStore>(
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

/// Get the latest commit ID from the store.
pub async fn latest_commit_id<M: MetaStore>(
    meta: &M,
) -> Result<Option<CommitId>, CommitError<M::Error>> {
    let commit = meta.latest_commit().await.map_err(CommitError::Meta)?;
    Ok(commit.map(|c| c.id))
}

/// Failures while resolving a revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError<M> {
    /// Metadata backend failure.
    Meta(M),
    /// No commit with this ID exists.
    UnknownCommit {
        /// Requested commit ID.
        id: CommitId,
    },
    /// No ref with this name exists.
    UnknownRef {
        /// Requested ref name.
        name: RefName,
    },
    /// The ref exists but names no commit yet.
    UnbornRef {
        /// Requested ref name.
        name: RefName,
    },
    /// The revision reads HEAD, but the store has no active ref.
    NoActiveRef,
    /// The revision reads HEAD, whose branch has no commit yet.
    UnbornHead {
        /// Branch HEAD names.
        branch: RefName,
    },
    /// A `~` step goes past a commit's parents.
    UnknownRevision {
        /// Revision as written.
        revision: String,
    },
}

impl<M> From<CommitError<M>> for ResolveError<M> {
    fn from(err: CommitError<M>) -> Self {
        match err {
            CommitError::Meta(err) => Self::Meta(err),
            CommitError::UnknownCommit { id } => Self::UnknownCommit { id },
        }
    }
}

impl<M: fmt::Debug> fmt::Display for ResolveError<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Meta(_) => write!(f, "commit metadata failed"),
            Self::UnknownCommit { id } => write!(f, "unknown commit: {id}"),
            Self::UnknownRef { name } => write!(f, "unknown ref: {name}"),
            Self::UnbornRef { name } => write!(f, "ref has no commit yet: {name}"),
            Self::NoActiveRef => write!(f, "HEAD names no ref: the store has no active ref"),
            Self::UnbornHead { branch } => write!(
                f,
                "your current branch '{branch}' does not have any commits yet"
            ),
            Self::UnknownRevision { revision } => write!(f, "unknown revision: {revision}"),
        }
    }
}

/// Resolve `revision` to a commit.
///
/// `HEAD` is the active ref's commit. A commit ID must name a recorded
/// commit, and a name is looked up as a branch; an unborn one has no commit
/// to point at yet. A `~<n>` step then follows the first parent `n`
/// times, as Git's `get_nth_ancestor` does; a step past a root commit is an
/// `UnknownRevision`. Parents are stored sorted by ID, so for a commit with
/// several the first is the lowest-ID one; commits sekai records have one.
pub async fn resolve_revision<M: MetaStore>(
    meta: &M,
    revision: &Revision,
) -> Result<CommitId, ResolveError<M::Error>> {
    let mut commit = match revision.base() {
        RevisionBase::Head => head_commit(meta).await?,
        RevisionBase::Commit(id) => known_commit(meta, *id).await?,
        RevisionBase::Name(name) => branch_commit(meta, name.clone()).await?,
    };
    for _ in 0..revision.ancestor() {
        let parents = commit_parents(meta, commit).await?;
        commit = *parents
            .first()
            .ok_or_else(|| ResolveError::UnknownRevision {
                revision: String::from(revision.as_str()),
            })?;
    }
    Ok(commit)
}

async fn head_commit<M: MetaStore>(meta: &M) -> Result<CommitId, ResolveError<M::Error>> {
    let head = meta
        .active_ref()
        .await
        .map_err(ResolveError::Meta)?
        .ok_or(ResolveError::NoActiveRef)?;
    head.commit.ok_or(ResolveError::UnbornHead {
        branch: head.name.name,
    })
}

async fn known_commit<M: MetaStore>(
    meta: &M,
    id: CommitId,
) -> Result<CommitId, ResolveError<M::Error>> {
    meta.lookup_commit(id)
        .await
        .map_err(ResolveError::Meta)?
        .map(|commit| commit.id)
        .ok_or(ResolveError::UnknownCommit { id })
}

async fn branch_commit<M: MetaStore>(
    meta: &M,
    name: RefName,
) -> Result<CommitId, ResolveError<M::Error>> {
    let found = meta
        .lookup_ref(&Ref::branch(name.clone()))
        .await
        .map_err(ResolveError::Meta)?;
    let Some(record) = found else {
        return Err(ResolveError::UnknownRef { name });
    };
    record.commit.ok_or(ResolveError::UnbornRef { name })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{MemMeta, block_on};
    use alloc::string::ToString;
    use sekai_util::CommitEntry;

    #[test]
    fn lists_in_local_sequence_order() {
        let mut meta = MemMeta::default();
        let first = block_on(meta.create_commit(300, &[])).unwrap();
        let second = block_on(meta.create_commit(100, &[])).unwrap();
        let listed = block_on(list_commits(&meta)).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, first);
        assert_eq!(listed[1].id, second);
    }

    fn revision(text: &str) -> Revision {
        Revision::parse(text).unwrap()
    }

    /// `root <- middle <- tip`, with `main` active on `tip`.
    fn linear_history() -> (MemMeta, [CommitId; 3]) {
        let mut meta = MemMeta::default();
        let root = block_on(meta.create_commit(100, &[])).unwrap();
        let middle = block_on(meta.create_commit(200, &[root])).unwrap();
        let tip = block_on(meta.create_commit(300, &[middle])).unwrap();
        let main = RefName::parse("main").unwrap();
        block_on(meta.create_ref(&Ref::branch(main.clone()), tip, 300)).unwrap();
        meta.with_active_ref(&main);
        (meta, [root, middle, tip])
    }

    #[test]
    fn revisions_walk_first_parents_from_head_ids_and_names() {
        let (meta, [root, middle, tip]) = linear_history();
        for (text, commit) in [
            ("HEAD", tip),
            ("@", tip),
            ("HEAD~", middle),
            ("@~1", middle),
            ("HEAD~2", root),
            ("main", tip),
            ("main~2", root),
            ("HEAD~0", tip),
        ] {
            assert_eq!(
                block_on(resolve_revision(&meta, &revision(text))),
                Ok(commit),
                "{text}"
            );
        }
        let middle_hex = middle.to_string();
        assert_eq!(
            block_on(resolve_revision(&meta, &revision(&middle_hex))),
            Ok(middle)
        );
        assert_eq!(
            block_on(resolve_revision(
                &meta,
                &revision(&alloc::format!("{middle_hex}~"))
            )),
            Ok(root)
        );
    }

    #[test]
    fn a_step_past_the_root_is_an_unknown_revision() {
        let (meta, _) = linear_history();
        let err = block_on(resolve_revision(&meta, &revision("HEAD~3"))).unwrap_err();
        assert_eq!(
            err,
            ResolveError::UnknownRevision {
                revision: "HEAD~3".into()
            }
        );
        assert_eq!(err.to_string(), "unknown revision: HEAD~3");
    }

    #[test]
    fn head_needs_a_born_active_ref() {
        let mut meta = MemMeta::default();
        assert_eq!(
            block_on(resolve_revision(&meta, &revision("HEAD"))),
            Err(ResolveError::NoActiveRef)
        );
        meta.with_active_ref(&RefName::parse("main").unwrap());
        let err = block_on(resolve_revision(&meta, &revision("@~1"))).unwrap_err();
        assert_eq!(
            err,
            ResolveError::UnbornHead {
                branch: RefName::parse("main").unwrap()
            }
        );
        assert_eq!(
            err.to_string(),
            "your current branch 'main' does not have any commits yet"
        );
    }

    #[test]
    fn revision_bases_report_what_is_missing() {
        let (meta, _) = linear_history();
        let unknown = "f".repeat(64);
        assert_eq!(
            block_on(resolve_revision(&meta, &revision(&unknown))),
            Err(ResolveError::UnknownCommit {
                id: CommitId::from_hex(unknown.as_bytes()).unwrap()
            })
        );
        assert_eq!(
            block_on(resolve_revision(&meta, &revision("other~1"))),
            Err(ResolveError::UnknownRef {
                name: RefName::parse("other").unwrap()
            })
        );
    }

    #[test]
    fn an_unborn_ref_resolves_to_nothing() {
        let mut meta = MemMeta::default();
        meta.with_active_ref(&RefName::parse("main").unwrap());
        assert_eq!(
            block_on(resolve_revision(&meta, &revision("main"))),
            Err(ResolveError::UnbornRef {
                name: RefName::parse("main").unwrap()
            })
        );
    }

    #[test]
    fn counts_fresh_and_effective_rows() {
        use sekai_util::{ChunkCoord, ChunkHash, Dimension, RegionKind};
        let mut meta = MemMeta::default();
        meta.with_active_ref(&RefName::parse("main").unwrap());
        let over = Dimension::OVERWORLD;
        let region = RegionKind::Region;
        let first = block_on(meta.apply_commit_incremental(
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
        let second = block_on(meta.apply_commit_incremental(
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
        meta.with_active_ref(&RefName::parse("main").unwrap());
        let over = Dimension::OVERWORLD;
        let nether = Dimension::NETHER;
        let region = RegionKind::Region;
        let entry = |dim, x, blob: Option<[u8; 32]>| {
            CommitEntry::new(ChunkCoord::new(dim, region, x, 0), blob.map(ChunkHash::new))
        };
        // Two overworld regions share one node by content, and the nether
        // starts its own dimension.
        let first = block_on(meta.apply_commit_incremental(
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
        let second = block_on(meta.apply_commit_incremental(
            200,
            &[first],
            &[entry(over, 0, Some([5; 32])), entry(over, 32, None)],
        ))
        .unwrap()
        .id;
        // Drop the nether's last chunk: the whole dimension vanishes.
        let third =
            block_on(meta.apply_commit_incremental(300, &[second], &[entry(nether, 0, None)]))
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
        let root = block_on(meta.create_commit(100, &[])).unwrap();
        let second = block_on(meta.create_commit(200, &[root])).unwrap();
        let third = block_on(meta.create_commit(300, &[second])).unwrap();

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
        meta.with_active_ref(&RefName::parse("main").unwrap());
        let coord = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
        let first_only = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Region, 1, 0);
        let first = block_on(meta.apply_commit_incremental(
            100,
            &[],
            &[
                CommitEntry::new(coord, Some(ChunkHash::new([1; 32]))),
                CommitEntry::new(first_only, Some(ChunkHash::new([3; 32]))),
            ],
        ))
        .unwrap()
        .id;
        // A second history: its root shares no ancestor with `first`. No
        // ref operation makes a ref unborn again, so the fake's open fields
        // stand in for one.
        let main = Ref::branch(RefName::parse("main").unwrap());
        if let Some(record) = meta.refs.get_mut(&main) {
            record.commit = None;
        }
        let other = block_on(meta.apply_commit_incremental(
            200,
            &[],
            &[CommitEntry::new(coord, Some(ChunkHash::new([2; 32])))],
        ))
        .unwrap()
        .id;
        let child = block_on(meta.apply_commit_incremental(300, &[other], &[]))
            .unwrap()
            .id;

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
        use crate::testing::MemError;
        let mut meta = MemMeta::default();
        let missing = CommitId([9; 32]);
        assert_eq!(
            block_on(meta.create_commit(100, &[missing])).unwrap_err(),
            MemError::UnknownParent { id: missing }
        );
        // A parent listed twice is recorded once.
        let root = block_on(meta.create_commit(100, &[])).unwrap();
        let child = block_on(meta.create_commit(200, &[root, root])).unwrap();
        assert_eq!(block_on(meta.commit_parents(child)).unwrap(), [root]);
    }

    #[test]
    fn repeating_a_commit_is_idempotent() {
        let mut meta = MemMeta::default();
        let first = block_on(meta.create_commit(100, &[])).unwrap();
        let again = block_on(meta.create_commit(100, &[])).unwrap();
        assert_eq!(first, again);
        assert_eq!(block_on(list_commits(&meta)).unwrap().len(), 1);
    }
}
