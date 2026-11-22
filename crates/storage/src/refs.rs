//! Ref objects: the store's named, movable pointers to commits.
//!
//! A ref names a commit and moves with the history it names. Exactly one
//! ref is active - the one new commits hang from. Reads here are pure
//! lookups; the mutations live in [`MetaStore`] and the app layer.

use alloc::string::String;
use alloc::vec::Vec;

use sekai_util::CommitRef;

use crate::graph::CommitError;
use crate::meta::MetaStore;

/// List all refs in full refname order (byte-wise, the order `Ref`
/// compares by).
pub async fn list_refs<M: MetaStore>(meta: &M) -> Result<Vec<CommitRef>, CommitError<M::Error>> {
    let mut out = Vec::new();
    meta.visit_refs(|r| {
        out.push(r.clone());
        true
    })
    .await
    .map_err(CommitError::Meta)?;
    Ok(out)
}

/// The active ref - the one new commits hang from - or `None` when the
/// store has no active ref row. A fresh store is not `None`: it starts
/// with an active, unborn ref whose `commit` is `None`.
pub async fn active_ref<M: MetaStore>(
    meta: &M,
) -> Result<Option<CommitRef>, CommitError<M::Error>> {
    meta.active_ref().await.map_err(CommitError::Meta)
}

/// The path of the store's main worktree, or `None` when none is
/// registered.
pub async fn main_worktree<M: MetaStore>(
    meta: &M,
) -> Result<Option<String>, CommitError<M::Error>> {
    let mut main = None;
    meta.visit_worktrees(|worktree| {
        if worktree.main {
            main = Some(worktree.path.clone());
            return false;
        }
        true
    })
    .await
    .map_err(CommitError::Meta)?;
    Ok(main)
}
