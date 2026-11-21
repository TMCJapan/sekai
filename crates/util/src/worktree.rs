//! Worktrees: the worlds a store backs up, each with its own HEAD.

use alloc::string::String;

use crate::commit::CommitId;
use crate::refs::RefName;

/// What a worktree's HEAD stores: a branch name or a commit.
///
/// Whether a branch HEAD is born depends on whether the branch exists,
/// which only the store knows; [`HeadState`] carries that answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    /// HEAD names a branch (`refs/heads/<name>`), which may be unborn.
    Branch(RefName),
    /// HEAD holds a commit directly.
    Detached(CommitId),
}

impl Head {
    /// Resolve this HEAD against the store. `branch_commit` is the
    /// commit the named branch points at, or `None` while the branch
    /// has none; a detached HEAD ignores it.
    pub fn resolve(&self, branch_commit: Option<CommitId>) -> HeadState {
        match self {
            Self::Branch(branch) => branch_commit.map_or_else(
                || HeadState::Unborn {
                    branch: branch.clone(),
                },
                |commit| HeadState::Attached {
                    branch: branch.clone(),
                    commit,
                },
            ),
            Self::Detached(commit) => HeadState::Detached { commit: *commit },
        }
    }
}

/// A worktree's HEAD resolved against the store's refs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadState {
    /// HEAD names a branch that points at a commit.
    Attached {
        /// Branch HEAD names.
        branch: RefName,
        /// Commit the branch points at.
        commit: CommitId,
    },
    /// HEAD names a branch that has no commit yet.
    Unborn {
        /// Branch the next commit lands on.
        branch: RefName,
    },
    /// HEAD holds a commit directly.
    Detached {
        /// Commit HEAD holds.
        commit: CommitId,
    },
}

impl HeadState {
    /// The commit HEAD stands at, or `None` while it is unborn.
    pub const fn commit(&self) -> Option<CommitId> {
        match self {
            Self::Attached { commit, .. } | Self::Detached { commit } => Some(*commit),
            Self::Unborn { .. } => None,
        }
    }
}

/// One world registered in a store, keyed by its world root.
///
/// Ordering and uniqueness are byte-wise and case-sensitive, over the
/// path alone: no separate name or ID exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// Absolute world root.
    pub path: String,
    /// Whether this is the main worktree: the world `init` registered.
    /// At most one worktree in a store is main.
    pub main: bool,
    /// What HEAD stores.
    pub head: Head,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_resolves_against_the_branch_commit() {
        let branch = RefName::parse("main").unwrap();
        let commit = CommitId([3; 32]);
        let named = Head::Branch(branch.clone());
        let attached = named.resolve(Some(commit));
        assert_eq!(
            attached,
            HeadState::Attached {
                branch: branch.clone(),
                commit,
            }
        );
        assert_eq!(attached.commit(), Some(commit));
        let unborn = named.resolve(None);
        assert_eq!(unborn, HeadState::Unborn { branch });
        assert_eq!(unborn.commit(), None);
        let detached = Head::Detached(commit).resolve(None);
        assert_eq!(detached, HeadState::Detached { commit });
        assert_eq!(detached.commit(), Some(commit));
    }
}
