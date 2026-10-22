//! Checkout planning for exact commit restoration.
//!
//! The plan contains present payloads grouped by region. Adapters rebuild
//! files from CAS blobs; missing blobs are treated as corruption.
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::fmt;

use crate::port::meta::MetaStore;
use sekai_util::{BlobHash, ChunkCoord, CommitId, RegionKey};

/// Outcome of one checkout run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckoutReport {
    /// Region files rewritten.
    pub files_written: usize,
    /// Region files deleted (post-commit or fully tombstoned).
    pub files_deleted: usize,
    /// Chunks restored from CAS blobs.
    pub chunks_restored: usize,
}

/// Resolved restore set for one commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutPlan {
    /// Commit creation time (unix millis, informational only).
    pub created_at_ms: u64,
    /// Present rows grouped by region.
    pub groups: BTreeMap<RegionKey, Vec<(ChunkCoord, BlobHash)>>,
    /// Tombstoned coordinates grouped by region. Regions absent from both
    /// `groups` and `tombstones` are unknown to the commit (created
    /// afterwards); regions in `tombstones` but not `groups` are fully
    /// tombstoned. Adapters need the distinction to apply keep-policies.
    pub tombstones: BTreeMap<RegionKey, Vec<ChunkCoord>>,
}

/// Failures while resolving a checkout plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutError<M> {
    /// Metadata backend failure.
    Meta(M),
    /// No commit with this ID exists.
    UnknownCommit {
        /// Requested commit ID.
        id: u64,
    },
}

impl<M: fmt::Debug> fmt::Display for CheckoutError<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Meta(_) => write!(f, "commit metadata failed"),
            Self::UnknownCommit { id } => write!(f, "unknown commit: {id}"),
        }
    }
}

/// Resolve present commit rows grouped by region.
pub async fn plan_checkout<M: MetaStore>(
    meta: &M,
    commit: CommitId,
) -> Result<CheckoutPlan, CheckoutError<M::Error>> {
    let created_at_ms = meta
        .lookup_commit(commit)
        .await
        .map_err(CheckoutError::Meta)?
        .map(|s| s.created_at_ms)
        .ok_or(CheckoutError::UnknownCommit { id: commit.0 })?;
    let mut groups: BTreeMap<RegionKey, Vec<(ChunkCoord, BlobHash)>> = BTreeMap::new();
    let mut tombstones: BTreeMap<RegionKey, Vec<ChunkCoord>> = BTreeMap::new();
    meta.visit_commit_chunks(commit, |entry| {
        if let Some(blob) = entry.blob {
            groups
                .entry(RegionKey::of(entry.coord))
                .or_default()
                .push((entry.coord, blob));
        } else {
            tombstones
                .entry(RegionKey::of(entry.coord))
                .or_default()
                .push(entry.coord);
        }
        true
    })
    .await
    .map_err(CheckoutError::Meta)?;
    Ok(CheckoutPlan {
        created_at_ms,
        groups,
        tombstones,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{MemMeta, block_on};
    use sekai_util::{CommitEntry, Dimension, RegionKind};

    const OVER: Dimension = Dimension::OVERWORLD;
    const REGION: RegionKind = RegionKind::Region;

    #[test]
    fn resolves_present_rows_grouped_by_region() {
        let mut meta = MemMeta::default();
        let a = ChunkCoord::new(OVER, REGION, 0, 0);
        let b = ChunkCoord::new(OVER, REGION, 1, 0);
        let gone = ChunkCoord::new(OVER, REGION, 2, 0);
        let id = block_on(meta.apply_commit_incremental(
            1_000,
            &[],
            &[
                CommitEntry::new(a, Some(BlobHash([1; 32])), None),
                CommitEntry::new(b, Some(BlobHash([2; 32])), None),
                CommitEntry::new(gone, None, None),
            ],
            None,
            &[],
            &[],
        ))
        .unwrap()
        .id;

        let plan = block_on(plan_checkout(&meta, id)).unwrap();
        assert_eq!(plan.created_at_ms, 1_000);
        assert_eq!(plan.groups.len(), 1);
        let rows = &plan.groups[&RegionKey::new(OVER, REGION, 0, 0)];
        assert_eq!(rows.len(), 2);
        assert!(rows.contains(&(a, BlobHash([1; 32]))));
        assert!(rows.contains(&(b, BlobHash([2; 32]))));
        assert_eq!(
            plan.tombstones[&RegionKey::new(OVER, REGION, 0, 0)],
            alloc::vec![gone]
        );
    }

    #[test]
    fn unknown_commit_is_an_error() {
        let meta = MemMeta::default();
        assert_eq!(
            block_on(plan_checkout(&meta, CommitId(7))),
            Err(CheckoutError::UnknownCommit { id: 7 })
        );
    }
}
