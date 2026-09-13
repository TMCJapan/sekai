//! Garbage-collection plan data.
//!
//! Rationale: blobs are immutable and global, so orphans only arise from
//! torn backups (CAS flushed, metadata never committed). Collection splits
//! into a pure plan (read-only candidate list, safe to preview or discard)
//! and an apply step (physical unlink) that re-verifies each candidate
//! against fresh metadata before removing. No metadata rows are touched:
//! orphans are unreferenced by definition, and snapshot pruning does not
//! exist.

use alloc::vec::Vec;

use super::hash::BlobHash;

/// Orphan-blob candidates: read-only, discardable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcPlan {
    /// Stored blobs referenced by no history row, in hash order.
    orphans: Vec<BlobHash>,
    /// Blobs present in CAS when planned.
    examined: usize,
}

impl GcPlan {
    /// Construct a plan from candidates already in hash order.
    #[must_use]
    pub fn new(mut orphans: Vec<BlobHash>, examined: usize) -> Self {
        orphans.sort();
        Self { orphans, examined }
    }

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

    /// Consume the plan into its candidate list.
    #[must_use]
    pub fn into_orphans(self) -> Vec<BlobHash> {
        self.orphans
    }
}
