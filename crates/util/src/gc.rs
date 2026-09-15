//! Garbage-collection plan and scan statistics.

use super::BlobHash;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gc_plan_accessors() {
        let empty = GcPlan::new(Vec::new(), 10);
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
        assert_eq!(empty.examined(), 10);
        assert!(empty.orphans().is_empty());

        let plan = GcPlan::new(alloc::vec![BlobHash([1; 32])], 3);
        assert!(!plan.is_empty());
        assert_eq!(plan.len(), 1);
        assert_eq!(plan.examined(), 3);
        assert_eq!(plan.orphans(), &[BlobHash([1; 32])]);
        assert_eq!(plan.into_orphans(), alloc::vec![BlobHash([1; 32])]);
    }
}
