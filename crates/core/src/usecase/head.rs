//! Reading a worktree's HEAD (ADR-0035).
//!
//! The store resolves HEAD against the refs; these helpers name the
//! failures and label HEAD for its reflog message.

use alloc::string::{String, ToString};

use sekai_storage::MetaStore;
use sekai_util::{CommitId, HeadState, RefName, WorktreeName};

use super::graph::is_ancestor;
use super::resolve::ResolveError;

/// `worktree`'s HEAD resolved against the refs, or `None` when the
/// worktree does not exist.
pub async fn head<M: MetaStore>(
    meta: &M,
    worktree: &WorktreeName,
) -> Result<Option<HeadState>, M::Error> {
    meta.head(worktree).await
}

/// How Git names where HEAD stands in a `checkout: moving from` message:
/// the branch, born or unborn, or the commit of a detached HEAD.
pub fn head_label(state: &HeadState) -> String {
    match state {
        HeadState::Attached { branch, .. } | HeadState::Unborn { branch } => branch.to_string(),
        HeadState::Detached { commit } => commit.to_string(),
    }
}

/// `worktree` and its HEAD; fails outside every worktree or for an
/// unknown one.
///
/// This is the only way from an optional worktree to its name. The store
/// reads an unknown worktree's HEAD log as empty, so anything that reads
/// HEAD goes through here and proves the worktree exists first.
pub async fn head_state<'a, M: MetaStore>(
    meta: &M,
    worktree: Option<&'a WorktreeName>,
) -> Result<(&'a WorktreeName, HeadState), ResolveError<M::Error>> {
    let worktree = worktree.ok_or(ResolveError::NoWorktree)?;
    let state = head(meta, worktree)
        .await
        .map_err(ResolveError::Meta)?
        .ok_or_else(|| ResolveError::UnknownWorktree {
            name: worktree.clone(),
        })?;
    Ok((worktree, state))
}

/// The commit HEAD stands at; fails while HEAD is unborn, outside every
/// worktree, or for an unknown one.
pub async fn born_head<M: MetaStore>(
    meta: &M,
    worktree: Option<&WorktreeName>,
) -> Result<CommitId, ResolveError<M::Error>> {
    match head_state(meta, worktree).await?.1 {
        HeadState::Attached { commit, .. } | HeadState::Detached { commit } => Ok(commit),
        HeadState::Unborn { branch } => Err(ResolveError::UnbornHead { branch }),
    }
}

/// The branch HEAD is on, born or unborn, or `None` when HEAD is
/// detached; fails outside every worktree or for an unknown one.
pub async fn head_branch<M: MetaStore>(
    meta: &M,
    worktree: Option<&WorktreeName>,
) -> Result<Option<RefName>, ResolveError<M::Error>> {
    Ok(head_state(meta, worktree).await?.1.branch().cloned())
}

/// Whether HEAD reaches `commit`, as `branch -d` asks before it deletes
/// a branch at `commit`.
///
/// An unborn HEAD reaches nothing, so Git's `branch -d` refuses every
/// branch then (git 2.53).
pub async fn head_reaches<M: MetaStore>(
    meta: &M,
    worktree: Option<&WorktreeName>,
    commit: CommitId,
) -> Result<bool, ResolveError<M::Error>> {
    match head_state(meta, worktree).await?.1.commit() {
        Some(head) => Ok(is_ancestor(meta, commit, head).await?),
        None => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{MemMeta, block_on, linear_history, main_worktree};

    #[test]
    fn head_reaches_its_history_only() {
        let (mut meta, [a, _, c]) = linear_history();
        let side = meta.create_commit(400, &[a]).unwrap();
        let main = main_worktree();
        let reaches = |commit| block_on(head_reaches(&meta, Some(&main), commit));
        assert_eq!(reaches(a), Ok(true));
        assert_eq!(reaches(c), Ok(true));
        assert_eq!(reaches(side), Ok(false));
        assert_eq!(
            block_on(head_reaches(&meta, None, a)),
            Err(ResolveError::NoWorktree)
        );
    }

    #[test]
    fn an_unborn_head_reaches_nothing() {
        let mut meta = MemMeta::default();
        meta.with_main();
        let commit = meta.create_commit(100, &[]).unwrap();
        assert_eq!(
            block_on(head_reaches(&meta, Some(&main_worktree()), commit)),
            Ok(false)
        );
    }
}
