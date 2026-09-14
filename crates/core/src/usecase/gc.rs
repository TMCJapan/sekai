//! Garbage collection over blob and metadata ports.
//!
//! Planning is read-only; applying re-checks candidates against fresh
//! metadata before removal.
use alloc::collections::BTreeSet;
use alloc::vec::Vec;
use core::fmt;

use crate::domain::gc::GcPlan;
use crate::domain::hash::BlobHash;
use crate::port::blob::BlobStore;
use crate::port::meta::MetaStore;

/// Outcome of one [`gc_apply`] run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GcReport {
    /// Candidates carried by the plan.
    pub candidates: usize,
    /// Candidates still orphan at apply time.
    pub orphans: usize,
    /// Blobs physically unlinked.
    pub removed: usize,
}

/// Failures while planning or applying garbage collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GcError<B, M> {
    /// Blob backend failure.
    Blob(B),
    /// Metadata backend failure.
    Meta(M),
}

impl<B: fmt::Debug, M: fmt::Debug> fmt::Display for GcError<B, M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blob(_) => write!(f, "blob store failed"),
            Self::Meta(_) => write!(f, "snapshot metadata failed"),
        }
    }
}

/// Find CAS blobs that are not referenced by any snapshot history row.
pub async fn gc_plan<B: BlobStore, M: MetaStore>(
    blobs: &B,
    meta: &M,
) -> Result<GcPlan, GcError<B::Error, M::Error>> {
    let mut referenced: BTreeSet<BlobHash> = BTreeSet::new();
    let mut snapshots: Vec<crate::domain::snapshot::SnapshotId> = Vec::new();
    meta.visit_snapshots(|snapshot| {
        snapshots.push(snapshot.id);
        true
    })
    .await
    .map_err(GcError::Meta)?;
    for id in snapshots {
        meta.visit_snapshot_chunks(id, |entry| {
            if let Some(blob) = entry.blob {
                referenced.insert(blob);
            }
            true
        })
        .await
        .map_err(GcError::Meta)?;
    }

    let mut orphans = Vec::new();
    let mut examined = 0usize;
    blobs
        .visit_blobs(|hash| {
            examined += 1;
            if !referenced.contains(hash) {
                orphans.push(*hash);
            }
            true
        })
        .await
        .map_err(GcError::Blob)?;
    Ok(GcPlan::new(orphans, examined))
}

/// Remove candidates that are still unreferenced after a fresh scan.
pub async fn gc_apply<B: BlobStore, M: MetaStore>(
    blobs: &mut B,
    meta: &M,
    plan: &GcPlan,
) -> Result<GcReport, GcError<B::Error, M::Error>> {
    let fresh = gc_plan(&*blobs, meta).await?;
    let still_orphan: BTreeSet<BlobHash> = fresh.into_orphans().into_iter().collect();
    let mut orphans = 0usize;
    let mut removed = 0usize;
    for hash in plan.orphans() {
        if !still_orphan.contains(hash) {
            continue;
        }
        orphans += 1;
        if blobs.remove(hash).await.map_err(GcError::Blob)? {
            removed += 1;
        }
    }
    Ok(GcReport {
        candidates: plan.len(),
        orphans,
        removed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::coords::{Dimension, RegionKind};
    use crate::domain::region::SnapshotEntry;
    use crate::support::{MemCas, MemMeta, block_on};

    const OVER: Dimension = Dimension::OVERWORLD;
    const REGION: RegionKind = RegionKind::REGION;

    fn coord(x: i32, z: i32) -> crate::domain::coords::ChunkCoord {
        crate::domain::coords::ChunkCoord::new(OVER, REGION, x, z)
    }

    #[test]
    fn reclaims_only_true_orphans() {
        let mut meta = MemMeta::default();
        let mut cas = MemCas::default();
        let live = BlobHash([1; 32]);
        let orphan = BlobHash([2; 32]);
        block_on(cas.put(&live, b"live")).unwrap();
        block_on(cas.put(&orphan, b"orphan")).unwrap();
        block_on(meta.apply_snapshot_incremental(
            1_000,
            &[SnapshotEntry::new(coord(0, 0), Some(live), None)],
            None,
            &[],
            &[],
        ))
        .unwrap();

        let plan = block_on(gc_plan(&cas, &meta)).unwrap();
        assert_eq!(plan.orphans(), &[orphan]);
        assert_eq!(plan.examined(), 2);

        let report = block_on(gc_apply(&mut cas, &meta, &plan)).unwrap();
        assert_eq!(
            report,
            GcReport {
                candidates: 1,
                orphans: 1,
                removed: 1,
            }
        );
        assert!(block_on(cas.contains(&live)).unwrap());
        assert!(!block_on(cas.contains(&orphan)).unwrap());
    }
}
