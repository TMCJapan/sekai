//! Garbage collection over abstract ports.
//!
//! Rationale: blobs are immutable and global, so orphans only arise from
//! torn backups (CAS flushed, metadata never committed). Collection splits
//! into a pure [`gc_plan`] (read-only candidate list, safe to preview or
//! discard) and [`gc_apply`] (physical unlink). [`gc_apply`] re-verifies
//! each candidate against fresh metadata before removing, so a blob
//! referenced after planning is never deleted. No metadata rows are
//! touched: orphans are unreferenced by definition, and snapshot pruning
//! does not exist yet.

use alloc::collections::BTreeSet;
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

/// Collect orphan blobs: stored in CAS but referenced by no history row.
///
/// Pure read: touches nothing on disk. Scope is global (all snapshots,
/// dimensions, and chunks), matching the global CAS deduplication.
pub fn gc_plan<B: BlobStore, M: MetaStore>(
    blobs: &B,
    meta: &M,
) -> Result<GcPlan, GcError<B::Error, M::Error>> {
    let mut referenced: BTreeSet<BlobHash> = BTreeSet::new();
    let mut failure: Option<M::Error> = None;
    meta.visit_snapshots(|snapshot| {
        if failure.is_some() {
            return false;
        }
        if let Err(err) = meta.visit_snapshot_chunks(snapshot.id, |entry| {
            if let Some(blob) = entry.blob {
                referenced.insert(blob);
            }
            true
        }) {
            failure = Some(err);
            return false;
        }
        true
    })
    .map_err(GcError::Meta)?;
    if let Some(err) = failure {
        return Err(GcError::Meta(err));
    }

    let mut orphans = alloc::vec::Vec::new();
    let mut examined = 0usize;
    blobs
        .visit_blobs(|hash| {
            examined += 1;
            if !referenced.contains(hash) {
                orphans.push(*hash);
            }
            true
        })
        .map_err(GcError::Blob)?;
    Ok(GcPlan::new(orphans, examined))
}

/// Unlink the blobs a [`GcPlan`] still finds orphan.
///
/// Each candidate is re-verified against fresh metadata first: candidates
/// referenced after planning are skipped, never deleted.
pub fn gc_apply<B: BlobStore, M: MetaStore>(
    blobs: &mut B,
    meta: &M,
    plan: &GcPlan,
) -> Result<GcReport, GcError<B::Error, M::Error>> {
    let fresh = gc_plan(&*blobs, meta)?;
    let still_orphan: BTreeSet<BlobHash> = fresh.into_orphans().into_iter().collect();
    let mut orphans = 0usize;
    let mut removed = 0usize;
    for hash in plan.orphans() {
        if !still_orphan.contains(hash) {
            continue;
        }
        orphans += 1;
        if blobs.remove(hash).map_err(GcError::Blob)? {
            removed += 1;
        }
    }
    Ok(GcReport {
        candidates: plan.len(),
        orphans,
        removed,
    })
}
