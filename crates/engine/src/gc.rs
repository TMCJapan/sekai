//! GC: reclaim orphan blobs behind a plan/apply seam.
//!
//! Rationale: blobs are immutable and global, so orphans only arise from
//! torn backups (CAS flushed, metadata never committed). Collection splits
//! into a pure `gc_plan` (read-only candidate list, safe to preview or
//! discard) and `gc_apply` (physical unlink). `gc_apply` re-verifies each
//! candidate against fresh metadata before removing, so a blob referenced
//! after planning is never deleted. No metadata rows are touched: orphans
//! are unreferenced by definition, and snapshot pruning does not exist yet.

use std::collections::HashSet;

use sekai_core::{BlobHash, MetaStore as _};

use crate::error::EngineError;
use crate::store::Store;

/// Orphan-blob candidates from [`gc_plan`]: read-only, discardable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcPlan {
    /// Stored blobs referenced by no history row, in hash order.
    orphans: Vec<BlobHash>,
    /// Blobs present in CAS when planned.
    examined: usize,
}

impl GcPlan {
    /// Candidate hashes to remove.
    #[must_use]
    pub fn orphans(&self) -> &[BlobHash] {
        &self.orphans
    }

    /// Blobs present in CAS when planned.
    #[must_use]
    pub const fn examined(&self) -> usize {
        self.examined
    }

    /// Number of candidates.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.orphans.len()
    }

    /// Whether no orphan was found.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.orphans.is_empty()
    }
}

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

/// Collect orphan blobs: stored in CAS but referenced by no history row.
///
/// Pure read: touches nothing on disk. Scope is global (all snapshots,
/// dimensions, and chunks), matching the global CAS deduplication.
pub fn gc_plan(store: &Store) -> Result<GcPlan, EngineError> {
    let mut referenced: HashSet<BlobHash> = HashSet::new();
    let mut failure: Option<EngineError> = None;
    store.meta().visit_snapshots(|snapshot| {
        if failure.is_some() {
            return false;
        }
        if let Err(err) = store.meta().visit_snapshot_chunks(snapshot.id, |entry| {
            if let Some(blob) = entry.blob {
                referenced.insert(blob);
            }
            true
        }) {
            failure = Some(EngineError::from(err));
            return false;
        }
        true
    })?;
    if let Some(err) = failure {
        return Err(err);
    }

    let mut orphans = Vec::new();
    let mut examined = 0usize;
    sekai_core::BlobStore::visit_blobs(store.cas(), |hash| {
        examined += 1;
        if !referenced.contains(hash) {
            orphans.push(*hash);
        }
        true
    })?;
    orphans.sort();
    Ok(GcPlan { orphans, examined })
}

/// Unlink the blobs a [`GcPlan`] still finds orphan.
///
/// Each candidate is re-verified against fresh metadata first: candidates
/// referenced after planning are skipped, never deleted.
pub fn gc_apply(store: &mut Store, plan: &GcPlan) -> Result<GcReport, EngineError> {
    let fresh = gc_plan(store)?;
    let still_orphan: HashSet<BlobHash> = fresh.orphans.into_iter().collect();
    let mut orphans = 0usize;
    let mut removed = 0usize;
    for hash in plan.orphans() {
        if !still_orphan.contains(hash) {
            continue;
        }
        orphans += 1;
        if sekai_core::BlobStore::remove(store.cas_mut(), hash)? {
            removed += 1;
        }
    }
    Ok(GcReport {
        candidates: plan.len(),
        orphans,
        removed,
    })
}
