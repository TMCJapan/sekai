//! Snapshot read operations.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;
use core::fmt;

use crate::port::meta::MetaStore;
use sekai_util::{Snapshot, SnapshotId, SnapshotTag, TagName};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotError<M> {
    /// Metadata backend failure.
    Meta(M),
    /// No snapshot with this ID exists.
    UnknownSnapshot {
        /// Requested snapshot ID.
        id: u64,
    },
}

impl<M: fmt::Debug> fmt::Display for SnapshotError<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Meta(_) => write!(f, "snapshot metadata failed"),
            Self::UnknownSnapshot { id } => write!(f, "unknown snapshot: {id}"),
        }
    }
}

/// List all snapshots in ID order.
pub async fn list_snapshots<M: MetaStore>(
    meta: &M,
) -> Result<Vec<Snapshot>, SnapshotError<M::Error>> {
    let mut out = Vec::new();
    meta.visit_snapshots(|snapshot| {
        out.push(snapshot.clone());
        true
    })
    .await
    .map_err(SnapshotError::Meta)?;
    Ok(out)
}

/// The snapshots a snapshot continues: its direct parents.
///
/// Fails when the snapshot does not exist: an unknown ID must not read
/// as the root of a history.
pub async fn snapshot_parents<M: MetaStore>(
    meta: &M,
    snapshot: SnapshotId,
) -> Result<Vec<SnapshotId>, SnapshotError<M::Error>> {
    ensure_exists(meta, snapshot).await?;
    meta.snapshot_parents(snapshot)
        .await
        .map_err(SnapshotError::Meta)
}

/// Every ancestor of `snapshot`, nearest first.
///
/// The walk follows parent links, so it covers the whole history the
/// snapshot descends from. The ancestors come by distance: the direct
/// parents first, then their parents, and so on; within one distance
/// they come in ascending ID order. Each snapshot is reported once.
///
/// A snapshot continues snapshots that are older than it, so the graph
/// is acyclic by construction; the visited set is the guard that keeps
/// the walk finite even if that ever changes.
pub async fn snapshot_ancestors<M: MetaStore>(
    meta: &M,
    snapshot: SnapshotId,
) -> Result<Vec<SnapshotId>, SnapshotError<M::Error>> {
    ensure_exists(meta, snapshot).await?;
    let mut seen: BTreeSet<SnapshotId> = BTreeSet::new();
    let mut order: Vec<SnapshotId> = Vec::new();
    let mut level: BTreeSet<SnapshotId> = meta
        .snapshot_parents(snapshot)
        .await
        .map_err(SnapshotError::Meta)?
        .into_iter()
        .collect();
    while !level.is_empty() {
        let mut next: BTreeSet<SnapshotId> = BTreeSet::new();
        for id in level {
            if !seen.insert(id) {
                continue;
            }
            order.push(id);
            for parent in meta
                .snapshot_parents(id)
                .await
                .map_err(SnapshotError::Meta)?
            {
                if !seen.contains(&parent) {
                    next.insert(parent);
                }
            }
        }
        level = next;
    }
    Ok(order)
}

/// The history of `snapshot`: the snapshot itself followed by its
/// ancestors, nearest first.
///
/// One walk reads the whole history: a record carries the parents the
/// walk follows. Every ancestor must have a record - a missing one is a
/// broken history, not a shorter history.
pub async fn snapshot_history<M: MetaStore>(
    meta: &M,
    snapshot: SnapshotId,
) -> Result<Vec<Snapshot>, SnapshotError<M::Error>> {
    let mut out = Vec::new();
    let mut seen: BTreeSet<SnapshotId> = BTreeSet::new();
    let mut level: BTreeSet<SnapshotId> = BTreeSet::from([snapshot]);
    while !level.is_empty() {
        let mut next: BTreeSet<SnapshotId> = BTreeSet::new();
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

/// Read one snapshot record; a missing record is a broken history.
async fn lookup_record<M: MetaStore>(
    meta: &M,
    snapshot: SnapshotId,
) -> Result<Snapshot, SnapshotError<M::Error>> {
    meta.lookup_snapshot(snapshot)
        .await
        .map_err(SnapshotError::Meta)?
        .ok_or(SnapshotError::UnknownSnapshot { id: snapshot.0 })
}

/// Fail when the snapshot does not exist.
async fn ensure_exists<M: MetaStore>(
    meta: &M,
    snapshot: SnapshotId,
) -> Result<(), SnapshotError<M::Error>> {
    lookup_record(meta, snapshot).await.map(|_| ())
}

/// List all tags in name order.
pub async fn list_tags<M: MetaStore>(
    meta: &M,
) -> Result<Vec<SnapshotTag>, SnapshotError<M::Error>> {
    let mut out = Vec::new();
    meta.visit_tags(|tag| {
        out.push(tag.clone());
        true
    })
    .await
    .map_err(SnapshotError::Meta)?;
    Ok(out)
}

/// Per-snapshot change statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotStats {
    /// Fresh present rows introduced at the snapshot.
    pub fresh_chunks: usize,
    /// Fresh tombstone rows introduced at the snapshot.
    pub fresh_tombstones: usize,
    /// Distinct new blobs among the fresh rows.
    pub new_blobs: usize,
    /// Effective present chunks at the snapshot (fallback included).
    pub effective_chunks: usize,
}

/// Count fresh and effective rows for one snapshot.
pub async fn snapshot_stats<M: MetaStore>(
    meta: &M,
    snapshot: SnapshotId,
) -> Result<SnapshotStats, SnapshotError<M::Error>> {
    use alloc::collections::BTreeSet;
    let mut fresh_chunks = 0usize;
    let mut fresh_tombstones = 0usize;
    let mut blobs = BTreeSet::new();
    meta.visit_fresh_rows(snapshot, |entry| {
        if let Some(blob) = entry.blob {
            fresh_chunks += 1;
            blobs.insert(blob);
        } else {
            fresh_tombstones += 1;
        }
        true
    })
    .await
    .map_err(SnapshotError::Meta)?;
    let mut effective_chunks = 0usize;
    meta.visit_snapshot_chunks(snapshot, |entry| {
        if entry.blob.is_some() {
            effective_chunks += 1;
        }
        true
    })
    .await
    .map_err(SnapshotError::Meta)?;
    Ok(SnapshotStats {
        fresh_chunks,
        fresh_tombstones,
        new_blobs: blobs.len(),
        effective_chunks,
    })
}

/// Get the latest snapshot ID from the store.
pub async fn latest_snapshot_id<M: MetaStore>(
    meta: &M,
) -> Result<Option<SnapshotId>, SnapshotError<M::Error>> {
    let snapshot = meta.latest_snapshot().await.map_err(SnapshotError::Meta)?;
    Ok(snapshot.map(|s| s.id))
}

/// Failures while resolving a snapshot reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError<M> {
    /// Metadata backend failure.
    Meta(M),
    /// No snapshot with this ID exists.
    UnknownSnapshot {
        /// Requested snapshot ID.
        id: u64,
    },
    /// No tag with this name exists.
    UnknownTag {
        /// Requested tag name.
        name: TagName,
    },
    /// Reference is neither a snapshot ID nor `@tag`.
    InvalidRef {
        /// Raw reference text.
        raw: alloc::string::String,
    },
}

impl<M: fmt::Debug> fmt::Display for ResolveError<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Meta(_) => write!(f, "snapshot metadata failed"),
            Self::UnknownSnapshot { id } => write!(f, "unknown snapshot: {id}"),
            Self::UnknownTag { name } => write!(f, "unknown tag: {name}"),
            Self::InvalidRef { raw } => write!(
                f,
                "invalid snapshot reference: {raw:?}, expected <id> or @<tag>"
            ),
        }
    }
}

/// Resolve a snapshot reference: a decimal ID, or `@tag` for a tag name.
pub async fn resolve_snapshot_ref<M: MetaStore>(
    meta: &M,
    raw: &str,
) -> Result<SnapshotId, ResolveError<M::Error>> {
    if let Some(name) = raw.strip_prefix('@') {
        let tag = TagName::parse(name).map_err(|_| ResolveError::InvalidRef {
            raw: alloc::string::String::from(raw),
        })?;
        return meta
            .lookup_tag(&tag)
            .await
            .map_err(ResolveError::Meta)?
            .map(|record| record.snapshot)
            .ok_or(ResolveError::UnknownTag { name: tag });
    }
    let id: u64 = raw.parse().map_err(|_| ResolveError::InvalidRef {
        raw: alloc::string::String::from(raw),
    })?;
    meta.lookup_snapshot(SnapshotId(id))
        .await
        .map_err(ResolveError::Meta)?
        .map(|snapshot| snapshot.id)
        .ok_or(ResolveError::UnknownSnapshot { id })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{MemMeta, block_on};
    use sekai_util::SnapshotTag;

    #[test]
    fn lists_in_id_order() {
        let mut meta = MemMeta::default();
        block_on(meta.create_snapshot(300, &[])).unwrap();
        block_on(meta.create_snapshot(100, &[])).unwrap();
        let listed = block_on(list_snapshots(&meta)).unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed[0].id < listed[1].id);
    }

    #[test]
    fn resolves_ids_and_tags() {
        use sekai_util::TagName;
        let mut meta = MemMeta::default();
        let first = block_on(meta.create_snapshot(100, &[])).unwrap();
        let second = block_on(meta.create_snapshot(200, &[first])).unwrap();
        block_on(meta.tag_snapshot(&TagName::parse("stable").unwrap(), second, 250)).unwrap();

        assert_eq!(block_on(resolve_snapshot_ref(&meta, "1")), Ok(first));
        assert_eq!(block_on(resolve_snapshot_ref(&meta, "@stable")), Ok(second));
        assert_eq!(
            block_on(resolve_snapshot_ref(&meta, "9")),
            Err(ResolveError::UnknownSnapshot { id: 9 })
        );
        assert_eq!(
            block_on(resolve_snapshot_ref(&meta, "@missing")),
            Err(ResolveError::UnknownTag {
                name: TagName::parse("missing").unwrap()
            })
        );
        assert!(matches!(
            block_on(resolve_snapshot_ref(&meta, "abc")),
            Err(ResolveError::InvalidRef { .. })
        ));
        assert!(matches!(
            block_on(resolve_snapshot_ref(&meta, "@123")),
            Err(ResolveError::InvalidRef { .. })
        ));
        assert!(matches!(
            block_on(resolve_snapshot_ref(&meta, "")),
            Err(ResolveError::InvalidRef { .. })
        ));
    }

    #[test]
    fn tags_round_trip_in_order() {
        use alloc::borrow::ToOwned;
        use sekai_util::TagName;
        let mut meta = MemMeta::default();
        let id = block_on(meta.create_snapshot(100, &[])).unwrap();
        for name in ["zeta", "alpha"] {
            block_on(meta.tag_snapshot(&TagName::parse(name).unwrap(), id, 150)).unwrap();
        }
        let mut seen = Vec::new();
        block_on(meta.visit_tags(|tag: &SnapshotTag| {
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
        let first = block_on(meta.create_snapshot(100, &[])).unwrap();
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
        let second = block_on(meta.create_snapshot(200, &[first])).unwrap();
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
            block_on(snapshot_stats(&meta, first)).unwrap(),
            SnapshotStats {
                fresh_chunks: 2,
                fresh_tombstones: 0,
                new_blobs: 1,
                effective_chunks: 2,
            }
        );
        assert_eq!(
            block_on(snapshot_stats(&meta, second)).unwrap(),
            SnapshotStats {
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
        let root = block_on(meta.create_snapshot(100, &[])).unwrap();
        let second = block_on(meta.create_snapshot(200, &[root])).unwrap();
        let third = block_on(meta.create_snapshot(300, &[second])).unwrap();

        assert_eq!(
            block_on(snapshot_ancestors(&meta, third)).unwrap(),
            [second, root]
        );
        assert_eq!(
            block_on(snapshot_ancestors(&meta, root)).unwrap(),
            [] as [SnapshotId; 0]
        );

        let walked = block_on(snapshot_history(&meta, third)).unwrap();
        assert_eq!(
            walked.iter().map(|s| s.id).collect::<Vec<_>>(),
            [third, second, root]
        );
        assert_eq!(walked[0].parents, [second]);
        assert!(walked[2].is_root());
    }

    #[test]
    fn a_snapshot_only_inherits_from_its_own_ancestors() {
        use sekai_util::{BlobHash, ChunkCoord, Dimension, RegionKind};
        let mut meta = MemMeta::default();
        let coord = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
        let first = block_on(meta.create_snapshot(100, &[])).unwrap();
        block_on(meta.record_chunk(first, &coord, Some(&BlobHash([1; 32])), None)).unwrap();
        // A second history: its root shares no ancestor with `first`.
        let other = block_on(meta.create_snapshot(200, &[])).unwrap();
        let child = block_on(meta.create_snapshot(300, &[other])).unwrap();

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
    fn retiring_a_snapshot_hands_its_children_its_parents() {
        let mut meta = MemMeta::default();
        let root = block_on(meta.create_snapshot(100, &[])).unwrap();
        let second = block_on(meta.create_snapshot(200, &[root])).unwrap();
        let third = block_on(meta.create_snapshot(300, &[second])).unwrap();

        block_on(meta.retire_snapshot(second, third)).unwrap();

        // The retired snapshot leaves the graph; its child continues the
        // history of the retired snapshot's own parent.
        assert!(block_on(meta.lookup_snapshot(second)).unwrap().is_none());
        assert_eq!(block_on(meta.snapshot_parents(third)).unwrap(), [root]);
        assert_eq!(block_on(snapshot_ancestors(&meta, third)).unwrap(), [root]);
    }

    #[test]
    fn an_unknown_snapshot_is_not_a_root() {
        let meta = MemMeta::default();
        assert_eq!(
            block_on(snapshot_parents(&meta, SnapshotId(7))).unwrap_err(),
            SnapshotError::UnknownSnapshot { id: 7 }
        );
        assert_eq!(
            block_on(snapshot_ancestors(&meta, SnapshotId(7))).unwrap_err(),
            SnapshotError::UnknownSnapshot { id: 7 }
        );
        assert_eq!(
            block_on(snapshot_history(&meta, SnapshotId(7))).unwrap_err(),
            SnapshotError::UnknownSnapshot { id: 7 }
        );
    }

    #[test]
    fn a_snapshot_cannot_name_a_missing_or_itself_as_parent() {
        use crate::support::MemError;
        let mut meta = MemMeta::default();
        // The first snapshot takes id 1, so naming it is a self-reference.
        assert_eq!(
            block_on(meta.create_snapshot(100, &[SnapshotId(1)])).unwrap_err(),
            MemError::SelfParent { id: 1 }
        );
        assert_eq!(
            block_on(meta.create_snapshot(100, &[SnapshotId(9)])).unwrap_err(),
            MemError::UnknownParent { id: 9 }
        );
        // A parent listed twice is recorded once.
        let root = block_on(meta.create_snapshot(100, &[])).unwrap();
        let child = block_on(meta.create_snapshot(200, &[root, root])).unwrap();
        assert_eq!(block_on(meta.snapshot_parents(child)).unwrap(), [root]);
    }

    #[test]
    fn a_retirement_must_fold_into_a_descendant() {
        use crate::support::MemError;
        let mut meta = MemMeta::default();
        let first = block_on(meta.create_snapshot(100, &[])).unwrap();
        let second = block_on(meta.create_snapshot(200, &[first])).unwrap();
        let other = block_on(meta.create_snapshot(300, &[])).unwrap();

        // `other` is newer than `first` but descends from nothing:
        // folding into it would graft rows it never had.
        assert_eq!(
            block_on(meta.retire_snapshot(first, other)).unwrap_err(),
            MemError::InvalidRetirement {
                from: first.0,
                into: other.0
            }
        );
        // The successor in the same history is fine.
        assert!(block_on(meta.retire_snapshot(first, second)).is_ok());
    }

    #[test]
    fn identifiers_are_never_reused_after_a_retirement() {
        let mut meta = MemMeta::default();
        let first = block_on(meta.create_snapshot(100, &[])).unwrap();
        let second = block_on(meta.create_snapshot(200, &[first])).unwrap();
        block_on(meta.retire_snapshot(first, second)).unwrap();
        // The retired identifier is gone, but it is not handed out again:
        // history rows still reference it.
        let third = block_on(meta.create_snapshot(300, &[])).unwrap();
        assert!(third > second);
    }
}
