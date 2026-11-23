//! Ref objects: the store's named, movable pointers to commits.
//!
//! A ref names a commit and moves with the history it names. Exactly one
//! ref is active - the one new commits hang from. Reads here are pure
//! lookups; the mutations live in [`MetaStore`] and the app layer.

use alloc::vec::Vec;

use sekai_util::{CommitRef, RefKind};

use crate::graph::CommitError;
use crate::meta::MetaStore;

/// List the refs of `kind` in full refname order (byte-wise, the order
/// `Ref` compares by).
pub async fn list_refs_of<M: MetaStore>(
    meta: &M,
    kind: RefKind,
) -> Result<Vec<CommitRef>, CommitError<M::Error>> {
    let mut out = Vec::new();
    meta.visit_refs(|r| {
        if r.name.kind == kind {
            out.push(r.clone());
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{MemMeta, block_on};
    use sekai_util::{Ref, RefName};

    #[test]
    fn lists_refs_of_a_kind_in_refname_order() {
        let mut meta = MemMeta::default();
        let root = block_on(meta.create_commit(100, &[])).unwrap();
        let branch = |name| Ref::branch(RefName::parse(name).unwrap());
        for name in ["main", "feature/b", "feature/a"] {
            block_on(meta.create_ref(&branch(name), root, 100)).unwrap();
        }
        let listed: Vec<Ref> = block_on(list_refs_of(&meta, RefKind::Branch))
            .unwrap()
            .into_iter()
            .map(|record| record.name)
            .collect();
        assert_eq!(listed, ["feature/a", "feature/b", "main"].map(branch));
    }
}
