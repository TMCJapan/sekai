//! Worktrees: the worlds a store backs up, each with its own HEAD
//! (ADR-0035).

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
    /// The branch HEAD names, or `None` when HEAD is detached.
    pub const fn branch(&self) -> Option<&RefName> {
        match self {
            Self::Branch(name) => Some(name),
            Self::Detached(_) => None,
        }
    }

    /// Resolve this HEAD against the store. `branch_commit` is the
    /// commit the named branch points at, or `None` while the branch
    /// does not exist; a detached HEAD ignores it.
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
    /// HEAD names an existing branch.
    Attached {
        /// Branch HEAD names.
        branch: RefName,
        /// Commit the branch points at.
        commit: CommitId,
    },
    /// HEAD names a branch that has no commit yet.
    Unborn {
        /// Branch the next commit creates.
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

    /// The branch HEAD names, or `None` when HEAD is detached.
    pub const fn branch(&self) -> Option<&RefName> {
        match self {
            Self::Attached { branch, .. } | Self::Unborn { branch } => Some(branch),
            Self::Detached { .. } => None,
        }
    }
}

/// One world registered in a store, keyed by its world root (ADR-0035).
///
/// Ordering and uniqueness are byte-wise and case-sensitive, over the
/// path alone: no separate name or ID exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// Absolute world root. The main worktree's root is the directory
    /// holding the store.
    pub path: String,
    /// Whether this is the main worktree: the world that holds the store.
    /// At most one worktree in a store is main.
    pub main: bool,
    /// What HEAD stores.
    pub head: Head,
}

impl Worktree {
    /// Whether this is the main worktree: the world that holds the store.
    pub const fn is_main(&self) -> bool {
        self.main
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_states_expose_their_commit_and_branch() {
        let branch = RefName::parse("main").unwrap();
        let commit = CommitId([3; 32]);
        let attached = HeadState::Attached {
            branch: branch.clone(),
            commit,
        };
        let unborn = HeadState::Unborn {
            branch: branch.clone(),
        };
        let detached = HeadState::Detached { commit };
        assert_eq!(attached.commit(), Some(commit));
        assert_eq!(unborn.commit(), None);
        assert_eq!(detached.commit(), Some(commit));
        assert_eq!(attached.branch(), Some(&branch));
        assert_eq!(unborn.branch(), Some(&branch));
        assert_eq!(detached.branch(), None);
        assert_eq!(Head::Branch(branch.clone()).branch(), Some(&branch));
        assert_eq!(Head::Detached(commit).branch(), None);
    }

    #[test]
    fn head_resolves_against_the_branch_commit() {
        let branch = RefName::parse("main").unwrap();
        let commit = CommitId([3; 32]);
        let named = Head::Branch(branch.clone());
        assert_eq!(
            named.resolve(Some(commit)),
            HeadState::Attached {
                branch: branch.clone(),
                commit,
            }
        );
        assert_eq!(named.resolve(None), HeadState::Unborn { branch });
        assert_eq!(
            Head::Detached(commit).resolve(None),
            HeadState::Detached { commit }
        );
    }

    #[test]
    fn a_worktree_is_main_only_when_flagged() {
        let head = Head::Branch(RefName::parse("main").unwrap());
        let main = Worktree {
            path: String::from("/worlds/main"),
            main: true,
            head: head.clone(),
        };
        let linked = Worktree {
            path: String::from("/worlds/linked"),
            main: false,
            head,
        };
        assert!(main.is_main());
        assert!(!linked.is_main());
    }
}
