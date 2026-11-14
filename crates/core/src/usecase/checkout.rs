//! Checkout planning for exact commit restoration.
//!
//! The plan contains present payloads grouped by region. Adapters rebuild
//! files from CAS blobs; missing blobs are treated as corruption.
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::fmt;

use crate::port::meta::MetaStore;
use sekai_util::{ChunkCoord, ChunkHash, CommitId, RegionKey};

/// Outcome of one checkout run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckoutReport {
    /// Region files rewritten.
    pub files_written: usize,
    /// Region files deleted (absent from the commit's snapshot).
    pub files_deleted: usize,
    /// Chunks restored from CAS blobs.
    pub chunks_restored: usize,
}

/// Resolved restore set for one commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutPlan {
    /// Commit creation time (unix millis, informational only).
    pub created_at_ms: u64,
    /// Present rows grouped by region. A region absent here has no chunks at
    /// the commit: checkout deletes its file unless a keep-policy applies.
    pub groups: BTreeMap<RegionKey, Vec<(ChunkCoord, ChunkHash)>>,
}

/// Failures while resolving a checkout plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutError<M> {
    /// Metadata backend failure.
    Meta(M),
    /// No commit with this ID exists.
    UnknownCommit {
        /// Requested commit ID.
        id: CommitId,
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
        .map(|c| c.created_at_ms)
        .ok_or(CheckoutError::UnknownCommit { id: commit })?;
    let mut groups: BTreeMap<RegionKey, Vec<(ChunkCoord, ChunkHash)>> = BTreeMap::new();
    meta.visit_commit_chunks(commit, |entry| {
        if let Some(blob) = entry.blob {
            groups
                .entry(RegionKey::of(entry.coord))
                .or_default()
                .push((entry.coord, blob));
        }
        true
    })
    .await
    .map_err(CheckoutError::Meta)?;
    Ok(CheckoutPlan {
        created_at_ms,
        groups,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{MemMeta, block_on};
    use sekai_util::{CommitEntry, Dimension, RefName, RegionKind};

    const OVER: Dimension = Dimension::OVERWORLD;
    const REGION: RegionKind = RegionKind::Region;

    #[test]
    fn resolves_present_rows_grouped_by_region() {
        let mut meta = MemMeta::default();
        meta.with_active_ref(&RefName::parse("main").unwrap());
        let a = ChunkCoord::new(OVER, REGION, 0, 0);
        let b = ChunkCoord::new(OVER, REGION, 1, 0);
        let gone = ChunkCoord::new(OVER, REGION, 2, 0);
        let id = block_on(meta.apply_commit_incremental(
            1_000,
            &[],
            &[
                CommitEntry::new(a, Some(ChunkHash::new([1; 32]))),
                CommitEntry::new(b, Some(ChunkHash::new([2; 32]))),
                CommitEntry::new(gone, None),
            ],
        ))
        .unwrap()
        .id;

        let plan = block_on(plan_checkout(&meta, id)).unwrap();
        assert_eq!(plan.created_at_ms, 1_000);
        assert_eq!(plan.groups.len(), 1);
        let rows = &plan.groups[&RegionKey::new(OVER, REGION, 0, 0)];
        assert_eq!(rows.len(), 2);
        assert!(rows.contains(&(a, ChunkHash::new([1; 32]))));
        assert!(rows.contains(&(b, ChunkHash::new([2; 32]))));
        assert!(
            block_on(meta.lookup_chunk(id, &gone)).unwrap().is_none(),
            "absence is the tombstone"
        );
    }

    #[test]
    fn unknown_commit_is_an_error() {
        let meta = MemMeta::default();
        assert_eq!(
            block_on(plan_checkout(&meta, CommitId([7; 32]))),
            Err(CheckoutError::UnknownCommit {
                id: CommitId([7; 32])
            })
        );
    }
}
