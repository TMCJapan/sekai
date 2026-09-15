//! Rollback planning for exact snapshot restoration.
//!
//! The plan contains present payloads grouped by region. Adapters rebuild
//! files from CAS blobs; missing blobs are treated as corruption.
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::fmt;

use crate::port::meta::MetaStore;
use sekai_util::{BlobHash, ChunkCoord, RegionKey, SnapshotId};

/// Outcome of one rollback run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RollbackReport {
    /// Region files rewritten.
    pub files_written: usize,
    /// Region files deleted (post-snapshot or fully tombstoned).
    pub files_deleted: usize,
    /// Chunks restored from CAS blobs.
    pub chunks_restored: usize,
}

/// Resolved restore set for one snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackPlan {
    /// Snapshot creation time (unix millis, informational only).
    pub created_at_ms: u64,
    /// Present rows grouped by region.
    pub groups: BTreeMap<RegionKey, Vec<(ChunkCoord, BlobHash)>>,
}

/// Failures while resolving a rollback plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RollbackError<M> {
    /// Metadata backend failure.
    Meta(M),
    /// No snapshot with this ID exists.
    UnknownSnapshot {
        /// Requested snapshot ID.
        id: u64,
    },
}

impl<M: fmt::Debug> fmt::Display for RollbackError<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Meta(_) => write!(f, "snapshot metadata failed"),
            Self::UnknownSnapshot { id } => write!(f, "unknown snapshot: {id}"),
        }
    }
}

/// Resolve present snapshot rows grouped by region.
pub async fn plan_rollback<M: MetaStore>(
    meta: &M,
    snapshot: SnapshotId,
) -> Result<RollbackPlan, RollbackError<M::Error>> {
    let created_at_ms = meta
        .lookup_snapshot(snapshot)
        .await
        .map_err(RollbackError::Meta)?
        .map(|s| s.created_at_ms)
        .ok_or(RollbackError::UnknownSnapshot { id: snapshot.0 })?;
    let mut groups: BTreeMap<RegionKey, Vec<(ChunkCoord, BlobHash)>> = BTreeMap::new();
    meta.visit_snapshot_chunks(snapshot, |entry| {
        if let Some(blob) = entry.blob {
            groups
                .entry(RegionKey::of(entry.coord))
                .or_default()
                .push((entry.coord, blob));
        }
        true
    })
    .await
    .map_err(RollbackError::Meta)?;
    Ok(RollbackPlan {
        created_at_ms,
        groups,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{MemMeta, block_on};
    use sekai_util::{Dimension, RegionKind, SnapshotEntry};

    const OVER: Dimension = Dimension::OVERWORLD;
    const REGION: RegionKind = RegionKind::REGION;

    #[test]
    fn resolves_present_rows_grouped_by_region() {
        let mut meta = MemMeta::default();
        let a = ChunkCoord::new(OVER, REGION, 0, 0);
        let b = ChunkCoord::new(OVER, REGION, 1, 0);
        let gone = ChunkCoord::new(OVER, REGION, 2, 0);
        let id = block_on(meta.apply_snapshot_incremental(
            1_000,
            &[
                SnapshotEntry::new(a, Some(BlobHash([1; 32])), None),
                SnapshotEntry::new(b, Some(BlobHash([2; 32])), None),
                SnapshotEntry::new(gone, None, None),
            ],
            None,
            &[],
            &[],
        ))
        .unwrap()
        .id;

        let plan = block_on(plan_rollback(&meta, id)).unwrap();
        assert_eq!(plan.created_at_ms, 1_000);
        assert_eq!(plan.groups.len(), 1);
        let rows = &plan.groups[&RegionKey::new(OVER, REGION, 0, 0)];
        assert_eq!(rows.len(), 2);
        assert!(rows.contains(&(a, BlobHash([1; 32]))));
        assert!(rows.contains(&(b, BlobHash([2; 32]))));
    }

    #[test]
    fn unknown_snapshot_is_an_error() {
        let meta = MemMeta::default();
        assert_eq!(
            block_on(plan_rollback(&meta, SnapshotId(7))),
            Err(RollbackError::UnknownSnapshot { id: 7 })
        );
    }
}
