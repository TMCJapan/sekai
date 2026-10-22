//! Commit identifiers and metadata.

use alloc::vec::Vec;

use crate::tag::TagName;

/// Monotonically increasing commit identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommitId(pub u64);

impl CommitId {
    /// Raw ID (for display and integer columns).
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Recorded commit metadata.
///
/// A commit is a node of the history graph: `parents` names the
/// commits it continues. The list is never null - it is empty for the
/// root of a history, holds one entry for an ordinary commit, and
/// several for a merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    /// Commit identifier.
    pub id: CommitId,
    /// Creation time as Unix millis (informational only).
    pub created_at_ms: u64,
    /// Commits this one descends from.
    pub parents: Vec<CommitId>,
}

impl Commit {
    /// Construct commit metadata.
    pub const fn new(id: CommitId, created_at_ms: u64, parents: Vec<CommitId>) -> Self {
        Self {
            id,
            created_at_ms,
            parents,
        }
    }

    /// Whether the commit starts a history (no parents).
    pub const fn is_root(&self) -> bool {
        self.parents.is_empty()
    }
}

/// Named alias for one commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitTag {
    /// Validated tag name.
    pub name: TagName,
    /// Commit the tag points at.
    pub commit: CommitId,
    /// Tag creation time as Unix millis (informational only).
    pub created_at_ms: u64,
}

impl CommitTag {
    /// Construct a tag record.
    pub const fn new(name: TagName, commit: CommitId, created_at_ms: u64) -> Self {
        Self {
            name,
            commit,
            created_at_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_ids_and_metadata() {
        let id = CommitId(7);
        assert_eq!(id.raw(), 7);
        let commit = Commit::new(id, 1_700_000_000_000, alloc::vec![CommitId(6)]);
        assert_eq!(commit.id, id);
        assert_eq!(commit.created_at_ms, 1_700_000_000_000);
        assert_eq!(commit.parents, [CommitId(6)]);
        assert!(!commit.is_root());
        assert!(Commit::new(id, 0, alloc::vec![]).is_root());
        assert!(CommitId(1) < CommitId(2));
    }
}
