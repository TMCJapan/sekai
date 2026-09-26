//! Garbage-collection plan and scan statistics.

use super::BlobHash;
use alloc::vec::Vec;

/// Read-only orphan candidates and scan statistics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcPlan {
    /// Hashes safe to unlink.
    pub orphans: Vec<BlobHash>,
    /// Blob count examined while planning.
    pub examined: usize,
}

impl GcPlan {
    /// Construct a plan from collected candidates.
    pub const fn new(orphans: Vec<BlobHash>, examined: usize) -> Self {
        Self { orphans, examined }
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
    fn gc_plan_construction() {
        let empty = GcPlan::new(Vec::new(), 10);
        assert!(empty.orphans.is_empty());
        assert_eq!(empty.examined, 10);

        let plan = GcPlan::new(alloc::vec![BlobHash([1; 32])], 3);
        assert_eq!(plan.orphans, alloc::vec![BlobHash([1; 32])]);
        assert_eq!(plan.into_orphans(), alloc::vec![BlobHash([1; 32])]);
    }
}
