//! Worktrees: the worlds a store backs up, each with its own HEAD.

use alloc::string::String;

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
