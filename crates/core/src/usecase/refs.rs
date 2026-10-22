//! Ref objects: the store's named, movable pointers to commits.
//!
//! A ref names a commit and moves with the history it names. Exactly one
//! ref is active - the one new commits hang from. Reads here are pure
//! lookups; the mutations live in the storage port and the app layer.

use alloc::vec::Vec;

use crate::port::meta::MetaStore;
use sekai_util::CommitRef;

use super::graph::CommitError;

/// List all refs in name order.
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
/// store has none.
pub async fn active_ref<M: MetaStore>(
    meta: &M,
) -> Result<Option<CommitRef>, CommitError<M::Error>> {
    meta.active_ref().await.map_err(CommitError::Meta)
}
