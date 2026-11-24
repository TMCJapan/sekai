//! Worktrees: the worlds a store backs up, each with its own HEAD.

use crate::commit::CommitId;
use crate::refs::RefName;

/// What a worktree's HEAD stores: a branch name or a commit.
///
/// Whether a branch HEAD is born depends on whether the branch exists,
/// which only the store knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    /// HEAD names a branch (`refs/heads/<name>`), which may be unborn.
    Branch(RefName),
    /// HEAD holds a commit directly.
    Detached(CommitId),
}

/// A HEAD resolved against the refs: what [`Head`] stores, plus whether
/// the branch it names has a commit yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadState {
    /// HEAD names a branch that points at `commit`.
    Attached {
        /// The branch HEAD names.
        branch: RefName,
        /// The commit the branch points at.
        commit: CommitId,
    },
    /// HEAD names a branch with no commit yet: the next commit starts its
    /// history.
    Unborn {
        /// The branch HEAD names.
        branch: RefName,
    },
    /// HEAD holds a commit directly.
    Detached {
        /// The commit HEAD holds.
        commit: CommitId,
    },
}

impl HeadState {
    /// The commit HEAD stands on, or `None` while it is unborn.
    pub const fn commit(&self) -> Option<CommitId> {
        match self {
            Self::Attached { commit, .. } | Self::Detached { commit } => Some(*commit),
            Self::Unborn { .. } => None,
        }
    }

    /// The branch HEAD names, born or unborn; `None` when detached.
    pub const fn branch(&self) -> Option<&RefName> {
        match self {
            Self::Attached { branch, .. } | Self::Unborn { branch } => Some(branch),
            Self::Detached { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resolved_head_reports_its_commit_and_branch() {
        let main = RefName::parse("main").unwrap();
        let commit = CommitId([1; 32]);
        let attached = HeadState::Attached {
            branch: main.clone(),
            commit,
        };
        let unborn = HeadState::Unborn {
            branch: main.clone(),
        };
        let detached = HeadState::Detached { commit };

        assert_eq!(
            (attached.commit(), attached.branch()),
            (Some(commit), Some(&main))
        );
        assert_eq!((unborn.commit(), unborn.branch()), (None, Some(&main)));
        assert_eq!((detached.commit(), detached.branch()), (Some(commit), None));
    }
}
