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
