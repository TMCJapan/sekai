//! Commit-graph read operations: listing, parents, ancestors, history, and
//! reference resolution.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;
use core::fmt;

use crate::port::meta::MetaStore;
use sekai_util::{Commit, CommitId, CommitTag, RefName, TagName};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitError<M> {
    /// Metadata backend failure.
    Meta(M),
    /// No commit with this ID exists.
    UnknownCommit {
        /// Requested commit ID.
        id: u64,
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

/// List all commits in ID order.
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
/// they come in ascending ID order. Each commit is reported once.
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
        .ok_or(CommitError::UnknownCommit { id: commit.0 })
}

/// List all tags in name order.
pub async fn list_tags<M: MetaStore>(meta: &M) -> Result<Vec<CommitTag>, CommitError<M::Error>> {
    let mut out = Vec::new();
    meta.visit_tags(|tag| {
        out.push(tag.clone());
        true
    })
    .await
    .map_err(CommitError::Meta)?;
    Ok(out)
}

/// Per-commit change statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitStats {
    /// Fresh present rows introduced at the commit.
    pub fresh_chunks: usize,
    /// Fresh tombstone rows introduced at the commit.
    pub fresh_tombstones: usize,
    /// Distinct new blobs among the fresh rows.
    pub new_blobs: usize,
    /// Effective present chunks at the commit (fallback included).
    pub effective_chunks: usize,
}

/// Count fresh and effective rows for one commit.
pub async fn commit_stats<M: MetaStore>(
    meta: &M,
    commit: CommitId,
) -> Result<CommitStats, CommitError<M::Error>> {
    let mut fresh_chunks = 0usize;
    let mut fresh_tombstones = 0usize;
    let mut blobs = BTreeSet::new();
    meta.visit_fresh_rows(commit, |entry| {
        if let Some(blob) = entry.blob {
            fresh_chunks += 1;
            blobs.insert(blob);
        } else {
            fresh_tombstones += 1;
        }
        true
    })
    .await
    .map_err(CommitError::Meta)?;
    let mut effective_chunks = 0usize;
    meta.visit_commit_chunks(commit, |entry| {
        if entry.blob.is_some() {
            effective_chunks += 1;
        }
        true
    })
    .await
    .map_err(CommitError::Meta)?;
    Ok(CommitStats {
        fresh_chunks,
        fresh_tombstones,
        new_blobs: blobs.len(),
        effective_chunks,
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
        id: u64,
    },
    /// No tag with this name exists.
    UnknownTag {
        /// Requested tag name.
        name: TagName,
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
    /// Reference is neither a commit ID, a `@tag`, nor a ref name.
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
            Self::UnknownTag { name } => write!(f, "unknown tag: {name}"),
            Self::UnknownRef { name } => write!(f, "unknown ref: {name}"),
            Self::UnbornRef { name } => write!(f, "ref has no commit yet: {name}"),
            Self::InvalidRef { raw } => write!(
                f,
                "invalid reference: {raw:?}, expected <id>, @<tag>, or <ref>"
            ),
        }
    }
}

/// Resolve a commit reference: a decimal ID, `@tag` for a tag name, or a
/// bare ref name.
///
/// The `@` prefix is tag-only: a ref called `main` is written `main`,
/// and `@main` is an unknown tag, not that ref. A bare name is tried as
/// a decimal ID first, so `01` (and every other spelling `u64` accepts)
/// resolves to commit 1; anything else names a ref. Every valid ref name
/// holds a non-digit character, so the decimal branch can never shadow
/// one: a numeric spelling is either a commit ID or `UnknownCommit`,
/// never a ref. An unborn ref has no commit to point at yet.
pub async fn resolve_commit_ref<M: MetaStore>(
    meta: &M,
    raw: &str,
) -> Result<CommitId, ResolveError<M::Error>> {
    if let Some(name) = raw.strip_prefix('@') {
        let tag = TagName::parse(name).map_err(|_| ResolveError::InvalidRef {
            raw: alloc::string::String::from(raw),
        })?;
        return meta
            .lookup_tag(&tag)
            .await
            .map_err(ResolveError::Meta)?
            .map(|record| record.commit)
            .ok_or(ResolveError::UnknownTag { name: tag });
    }
    if let Ok(id) = raw.parse::<u64>() {
        return meta
            .lookup_commit(CommitId(id))
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
    use sekai_util::CommitTag;

    #[test]
    fn lists_in_id_order() {
        let mut meta = MemMeta::default();
        block_on(meta.create_commit(300, &[])).unwrap();
        block_on(meta.create_commit(100, &[])).unwrap();
        let listed = block_on(list_commits(&meta)).unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed[0].id < listed[1].id);
    }

    #[test]
    fn resolves_ids_tags_and_refs() {
        use sekai_util::TagName;
        let mut meta = MemMeta::default();
        let first = block_on(meta.create_commit(100, &[])).unwrap();
        let second = block_on(meta.create_commit(200, &[first])).unwrap();
        block_on(meta.tag_commit(&TagName::parse("stable").unwrap(), second, 250)).unwrap();
        let branch = RefName::parse("main").unwrap();
        block_on(meta.set_ref(&branch, Some(second), 300)).unwrap();

        assert_eq!(block_on(resolve_commit_ref(&meta, "1")), Ok(first));
        // `u64` accepts leading zeroes and a leading `+`; both spellings
        // resolve as IDs, and no valid ref name can take that shape.
        assert_eq!(block_on(resolve_commit_ref(&meta, "01")), Ok(first));
        assert_eq!(block_on(resolve_commit_ref(&meta, "+1")), Ok(first));
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
        assert_eq!(block_on(resolve_commit_ref(&meta, "@stable")), Ok(second));
        assert_eq!(block_on(resolve_commit_ref(&meta, "main")), Ok(second));
        assert_eq!(
            block_on(resolve_commit_ref(&meta, "9")),
            Err(ResolveError::UnknownCommit { id: 9 })
        );
        assert_eq!(
            block_on(resolve_commit_ref(&meta, "@missing")),
            Err(ResolveError::UnknownTag {
                name: TagName::parse("missing").unwrap()
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
            block_on(resolve_commit_ref(&meta, "")),
            Err(ResolveError::InvalidRef { .. })
        ));
    }

    #[test]
    fn numeric_spellings_are_commits_not_ref_names() {
        use sekai_util::RefNameError;

        // The resolver tries a decimal ID before a ref. `u64::from_str`
        // accepts these spellings; `RefName` rejects digit-only names, so
        // the two namespaces cannot overlap.
        for raw in ["123", "01", "+1"] {
            assert!(raw.parse::<u64>().is_ok());
        }
        assert_eq!(RefName::parse("123"), Err(RefNameError::Numeric));

        let meta = MemMeta::default();
        assert_eq!(
            block_on(resolve_commit_ref(&meta, "123")),
            Err(ResolveError::UnknownCommit { id: 123 })
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
    fn tags_round_trip_in_order() {
        use alloc::borrow::ToOwned;
        use sekai_util::TagName;
        let mut meta = MemMeta::default();
        let id = block_on(meta.create_commit(100, &[])).unwrap();
        for name in ["zeta", "alpha"] {
            block_on(meta.tag_commit(&TagName::parse(name).unwrap(), id, 150)).unwrap();
        }
        let mut seen = Vec::new();
        block_on(meta.visit_tags(|tag: &CommitTag| {
            seen.push(tag.name.as_str().to_owned());
            true
        }))
        .unwrap();
        assert_eq!(seen, ["alpha", "zeta"]);
        assert!(block_on(meta.untag(&TagName::parse("alpha").unwrap())).unwrap());
        assert!(!block_on(meta.untag(&TagName::parse("alpha").unwrap())).unwrap());
    }

    #[test]
    fn counts_fresh_and_effective_rows() {
        use sekai_util::{BlobHash, ChunkCoord, Dimension, RegionKind};
        let mut meta = MemMeta::default();
        let over = Dimension::OVERWORLD;
        let region = RegionKind::Region;
        let first = block_on(meta.create_commit(100, &[])).unwrap();
        block_on(meta.record_chunk(
            first,
            &ChunkCoord::new(over, region, 0, 0),
            Some(&BlobHash([1; 32])),
            None,
        ))
        .unwrap();
        block_on(meta.record_chunk(
            first,
            &ChunkCoord::new(over, region, 1, 0),
            Some(&BlobHash([1; 32])),
            None,
        ))
        .unwrap();
        let second = block_on(meta.create_commit(200, &[first])).unwrap();
        block_on(meta.record_chunk(
            second,
            &ChunkCoord::new(over, region, 0, 0),
            Some(&BlobHash([2; 32])),
            None,
        ))
        .unwrap();
        block_on(meta.record_chunk(second, &ChunkCoord::new(over, region, 1, 0), None, None))
            .unwrap();

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
        use sekai_util::{BlobHash, ChunkCoord, Dimension, RegionKind};
        let mut meta = MemMeta::default();
        let coord = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
        let first = block_on(meta.create_commit(100, &[])).unwrap();
        block_on(meta.record_chunk(first, &coord, Some(&BlobHash([1; 32])), None)).unwrap();
        // A second history: its root shares no ancestor with `first`.
        let other = block_on(meta.create_commit(200, &[])).unwrap();
        let child = block_on(meta.create_commit(300, &[other])).unwrap();

        assert!(
            block_on(meta.lookup_chunk(child, &coord))
                .unwrap()
                .is_none()
        );
        assert!(
            block_on(meta.lookup_chunk(first, &coord))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn retiring_a_commit_hands_its_children_its_parents() {
        let mut meta = MemMeta::default();
        let root = block_on(meta.create_commit(100, &[])).unwrap();
        let second = block_on(meta.create_commit(200, &[root])).unwrap();
        let third = block_on(meta.create_commit(300, &[second])).unwrap();

        block_on(meta.retire_commit(second, third)).unwrap();

        // The retired commit leaves the graph; its child continues the
        // history of the retired commit's own parent.
        assert!(block_on(meta.lookup_commit(second)).unwrap().is_none());
        assert_eq!(block_on(meta.commit_parents(third)).unwrap(), [root]);
        assert_eq!(block_on(commit_ancestors(&meta, third)).unwrap(), [root]);
    }

    #[test]
    fn an_unknown_commit_is_not_a_root() {
        let meta = MemMeta::default();
        assert_eq!(
            block_on(commit_parents(&meta, CommitId(7))).unwrap_err(),
            CommitError::UnknownCommit { id: 7 }
        );
        assert_eq!(
            block_on(commit_ancestors(&meta, CommitId(7))).unwrap_err(),
            CommitError::UnknownCommit { id: 7 }
        );
        assert_eq!(
            block_on(commit_history(&meta, CommitId(7))).unwrap_err(),
            CommitError::UnknownCommit { id: 7 }
        );
    }

    #[test]
    fn a_commit_cannot_name_a_missing_or_itself_as_parent() {
        use crate::support::MemError;
        let mut meta = MemMeta::default();
        // The first commit takes id 1, so naming it is a self-reference.
        assert_eq!(
            block_on(meta.create_commit(100, &[CommitId(1)])).unwrap_err(),
            MemError::SelfParent { id: 1 }
        );
        assert_eq!(
            block_on(meta.create_commit(100, &[CommitId(9)])).unwrap_err(),
            MemError::UnknownParent { id: 9 }
        );
        // A parent listed twice is recorded once.
        let root = block_on(meta.create_commit(100, &[])).unwrap();
        let child = block_on(meta.create_commit(200, &[root, root])).unwrap();
        assert_eq!(block_on(meta.commit_parents(child)).unwrap(), [root]);
    }

    #[test]
    fn a_retirement_must_fold_into_a_descendant() {
        use crate::support::MemError;
        let mut meta = MemMeta::default();
        let first = block_on(meta.create_commit(100, &[])).unwrap();
        let second = block_on(meta.create_commit(200, &[first])).unwrap();
        let other = block_on(meta.create_commit(300, &[])).unwrap();

        // `other` is newer than `first` but descends from nothing:
        // folding into it would graft rows it never had.
        assert_eq!(
            block_on(meta.retire_commit(first, other)).unwrap_err(),
            MemError::InvalidRetirement {
                from: first.0,
                into: other.0
            }
        );
        // The successor in the same history is fine.
        assert!(block_on(meta.retire_commit(first, second)).is_ok());
    }

    #[test]
    fn identifiers_are_never_reused_after_a_retirement() {
        let mut meta = MemMeta::default();
        let first = block_on(meta.create_commit(100, &[])).unwrap();
        let second = block_on(meta.create_commit(200, &[first])).unwrap();
        block_on(meta.retire_commit(first, second)).unwrap();
        // The retired identifier is gone, but it is not handed out again:
        // history rows still reference it.
        let third = block_on(meta.create_commit(300, &[])).unwrap();
        assert!(third > second);
    }
}
