//! Garbage-collection plan and scan statistics.

use super::hash::BlobHash;
use alloc::vec::Vec;

/// Read-only orphan candidates and scan statistics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcPlan {
    pub orphans: Vec<BlobHash>,
    pub examined: usize,
}

impl GcPlan {
    /// Construct a plan from collected candidates.
    pub const fn new(orphans: Vec<BlobHash>, examined: usize) -> Self {
        Self { orphans, examined }
    }

    /// Candidate count.
    pub const fn len(&self) -> usize {
        self.orphans.len()
    }

    /// Whether no candidates were found.
    pub const fn is_empty(&self) -> bool {
        self.orphans.is_empty()
    }

    /// Candidate hashes.
    pub fn orphans(&self) -> &[BlobHash] {
        &self.orphans
    }

    /// Blob count examined while planning.
    pub const fn examined(&self) -> usize {
        self.examined
    }

    /// Consume into the candidate list.
    pub fn into_orphans(self) -> Vec<BlobHash> {
        self.orphans
    }
}
