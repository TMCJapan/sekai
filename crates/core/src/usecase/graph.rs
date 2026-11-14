//! Commit-graph read operations: listing, parents, ancestors, history, and
//! reference resolution.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;
use core::fmt;

use crate::port::meta::MetaStore;
use sekai_util::{ChunkCoord, ChunkHash, Commit, CommitId, RefName};

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

/// Count fresh and effective rows for one commit.
///
/// Fresh rows are the tree diff against the first parent (the base the tree
/// merge would have used); a root commit diffs against the empty snapshot.
pub async fn commit_stats<M: MetaStore>(
    meta: &M,
    commit: CommitId,
) -> Result<CommitStats, CommitError<M::Error>> {
    let record = lookup_record(meta, commit).await?;
    let mut current: BTreeMap<ChunkCoord, ChunkHash> = BTreeMap::new();
    meta.visit_commit_chunks(commit, |entry| {
        if let Some(blob) = entry.blob {
            current.insert(entry.coord, blob);
        }
        true
    })
    .await
    .map_err(CommitError::Meta)?;

    let mut previous: BTreeMap<ChunkCoord, ChunkHash> = BTreeMap::new();
    if let Some(parent) = record.parents.first().copied() {
        meta.visit_commit_chunks(parent, |entry| {
            if let Some(blob) = entry.blob {
                previous.insert(entry.coord, blob);
            }
            true
        })
        .await
        .map_err(CommitError::Meta)?;
    }

    let mut fresh_chunks = 0usize;
    let mut blobs = BTreeSet::new();
    for (coord, hash) in &current {
        if previous.get(coord) != Some(hash) {
            fresh_chunks += 1;
            blobs.insert(*hash);
        }
    }
    let fresh_tombstones = previous
        .keys()
        .filter(|coord| !current.contains_key(coord))
        .count();
    Ok(CommitStats {
        fresh_chunks,
        fresh_tombstones,
        new_blobs: blobs.len(),
        effective_chunks: current.len(),
    })
}

/// Get the latest commit ID from the store.
pub async fn latest_commit_id<M: MetaStore>(
    meta: &M,
) -> Result<Option<CommitId>, CommitError<M::Error>> {
    let commit = meta.latest_commit().await.map_err(CommitError::Meta)?;
    Ok(commit.map(|c| c.id))
}

/// Failures while resolving a commit reference.
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
    /// Reference is neither a commit ID nor a ref name.
    InvalidRef {
        /// Raw reference text.
        raw: alloc::string::String,
    },
}

impl<M: fmt::Debug> fmt::Display for ResolveError<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Meta(_) => write!(f, "commit metadata failed"),
            Self::UnknownCommit { id } => write!(f, "unknown commit: {id}"),
            Self::UnknownRef { name } => write!(f, "unknown ref: {name}"),
            Self::UnbornRef { name } => write!(f, "ref has no commit yet: {name}"),
            Self::InvalidRef { raw } => write!(
                f,
                "invalid reference: {raw:?}, expected <64-char commit hash> or <ref>"
            ),
        }
    }
}

/// Resolve a commit reference: a 64-character hexadecimal commit ID or a
/// bare ref name.
///
/// A bare name is tried as a commit ID first: a 64-character hexadecimal
/// spelling is either a commit ID or `UnknownCommit`, never a ref, so a ref
/// whose name is exactly 64 hex characters is shadowed. An unborn ref has
/// no commit to point at yet.
pub async fn resolve_commit_ref<M: MetaStore>(
    meta: &M,
    raw: &str,
) -> Result<CommitId, ResolveError<M::Error>> {
    if let Ok(id) = raw.parse::<CommitId>() {
        return meta
            .lookup_commit(id)
            .await
            .map_err(ResolveError::Meta)?
            .map(|commit| commit.id)
            .ok_or(ResolveError::UnknownCommit { id });
    }
    let name = RefName::parse(raw).map_err(|_| ResolveError::InvalidRef {
        raw: alloc::string::String::from(raw),
    })?;
    let Some(record) = meta.lookup_ref(&name).await.map_err(ResolveError::Meta)? else {
        return Err(ResolveError::UnknownRef { name });
    };
    record.commit.ok_or(ResolveError::UnbornRef { name })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{MemMeta, block_on};
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

    #[test]
    fn resolves_hashes_and_refs() {
        let mut meta = MemMeta::default();
        let first = block_on(meta.create_commit(100, &[])).unwrap();
        let second = block_on(meta.create_commit(200, &[first])).unwrap();
        let branch = RefName::parse("main").unwrap();
        block_on(meta.set_ref(&branch, Some(second), 300)).unwrap();

        let first_hex = first.to_string();
        assert_eq!(first_hex.len(), 64);
        assert_eq!(block_on(resolve_commit_ref(&meta, &first_hex)), Ok(first));
        assert_eq!(block_on(resolve_commit_ref(&meta, "main")), Ok(second));
        assert_eq!(
            block_on(resolve_commit_ref(&meta, &"f".repeat(64))),
            Err(ResolveError::UnknownCommit {
                id: CommitId::from_hex("f".repeat(64).as_bytes()).unwrap()
            })
        );
        assert_eq!(
            block_on(resolve_commit_ref(&meta, " 1")),
            Err(ResolveError::InvalidRef { raw: " 1".into() })
        );
        assert_eq!(
            block_on(resolve_commit_ref(&meta, "")),
            Err(ResolveError::InvalidRef {
                raw: alloc::string::String::new()
            })
        );
        assert_eq!(
            block_on(resolve_commit_ref(&meta, "other")),
            Err(ResolveError::UnknownRef {
                name: RefName::parse("other").unwrap()
            })
        );
        assert!(matches!(
            block_on(resolve_commit_ref(&meta, "a b")),
            Err(ResolveError::InvalidRef { .. })
        ));
        assert!(matches!(
            block_on(resolve_commit_ref(&meta, "@123")),
            Err(ResolveError::InvalidRef { .. })
        ));
        assert!(matches!(
            block_on(resolve_commit_ref(&meta, "@main")),
            Err(ResolveError::InvalidRef { .. })
        ));
    }

    #[test]
    fn a_hex_ref_name_is_shadowed_by_commit_resolution() {
        // A 64-character hex spelling always resolves as a commit ID: an
        // unknown one is `UnknownCommit`, never the ref of the same name.
        let mut meta = MemMeta::default();
        let hex = "a".repeat(64);
        let name = RefName::parse(&hex).unwrap();
        block_on(meta.set_ref(&name, None, 100)).unwrap();
        assert_eq!(
            block_on(resolve_commit_ref(&meta, &hex)),
            Err(ResolveError::UnknownCommit {
                id: CommitId::from_hex(hex.as_bytes()).unwrap()
            })
        );
    }

    #[test]
    fn an_unborn_ref_resolves_to_nothing() {
        let mut meta = MemMeta::default();
        let branch = RefName::parse("main").unwrap();
        block_on(meta.set_ref(&branch, None, 100)).unwrap();
        assert_eq!(
            block_on(resolve_commit_ref(&meta, "main")),
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
        // A second history: its root shares no ancestor with `first`.
        block_on(meta.set_ref(&RefName::parse("main").unwrap(), None, 150)).unwrap();
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
        use crate::support::MemError;
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
