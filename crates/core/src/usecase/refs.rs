//! Refs and worktree HEADs (ADR-0035).
//!
//! Reads here are pure lookups; the mutations live in `MetaStore` and the
//! app layer.

use alloc::vec::Vec;

use sekai_storage::MetaStore;
use sekai_util::{CommitRef, HeadState, WorktreeName};

use super::graph::CommitError;

/// List all refs in full refname order (byte-wise).
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

/// `worktree`'s HEAD resolved against the refs, or `None` when the
/// worktree does not exist.
pub async fn head<M: MetaStore>(
    meta: &M,
    worktree: &WorktreeName,
) -> Result<Option<HeadState>, CommitError<M::Error>> {
    meta.head(worktree).await.map_err(CommitError::Meta)
}
