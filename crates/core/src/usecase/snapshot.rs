//! Snapshot read operations.

use core::fmt;

use crate::port::meta::MetaStore;
use alloc::vec::Vec;
use sekai_util::{Snapshot, SnapshotId, SnapshotTag, TagName};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotError<M> {
    /// Metadata backend failure.
    Meta(M),
}

impl<M: fmt::Debug> fmt::Display for SnapshotError<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Meta(_) => write!(f, "snapshot metadata failed"),
        }
    }
}

/// List all snapshots in ID order.
pub async fn list_snapshots<M: MetaStore>(
    meta: &M,
) -> Result<Vec<Snapshot>, SnapshotError<M::Error>> {
    let mut out = Vec::new();
    meta.visit_snapshots(|snapshot| {
        out.push(*snapshot);
        true
    })
    .await
    .map_err(SnapshotError::Meta)?;
    Ok(out)
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
        block_on(meta.create_snapshot(300)).unwrap();
        block_on(meta.create_snapshot(100)).unwrap();
        let listed = block_on(list_snapshots(&meta)).unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed[0].id < listed[1].id);
    }

    #[test]
    fn resolves_ids_and_tags() {
        use sekai_util::TagName;
        let mut meta = MemMeta::default();
        let first = block_on(meta.create_snapshot(100)).unwrap();
        let second = block_on(meta.create_snapshot(200)).unwrap();
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
        let id = block_on(meta.create_snapshot(100)).unwrap();
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
        let region = RegionKind::REGION;
        let first = block_on(meta.create_snapshot(100)).unwrap();
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
        let second = block_on(meta.create_snapshot(200)).unwrap();
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
}
