//! Ref objects: the store's named, movable pointers to commits.
//!
//! A ref names a commit and moves with the history it names. Each
//! worktree's HEAD names a branch (or holds a commit), and new commits land
//! on the HEAD of the worktree the store was opened from. Reads here are
//! pure lookups; the mutations live in [`MetaStore`] and the app layer.

use alloc::vec::Vec;

use sekai_util::{
    CommitRef, Ref, RefKind,
    worktree::{Head, HeadState},
};

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

/// The HEAD of the worktree the store was opened from, resolved against
/// the refs (see [`resolve_head`]).
pub async fn head_state<M: MetaStore>(meta: &M) -> Result<HeadState, M::Error> {
    resolve_head(meta, meta.head().await?).await
}

/// `head`, any worktree's, resolved against the refs: a branch without a
/// commit is unborn, as in a fresh store.
pub async fn resolve_head<M: MetaStore>(meta: &M, head: Head) -> Result<HeadState, M::Error> {
    match head {
        Head::Branch(branch) => {
            let record = meta.lookup_ref(&Ref::branch(branch.clone())).await?;
            Ok(match record.and_then(|record| record.commit) {
                Some(commit) => HeadState::Attached { branch, commit },
                None => HeadState::Unborn { branch },
            })
        }
        Head::Detached(commit) => Ok(HeadState::Detached { commit }),
    }
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
    use sekai_util::{CommitId, RefName};

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

    #[test]
    fn head_resolves_against_the_refs() {
        let mut meta = MemMeta::default();
        let main = RefName::parse("main").unwrap();
        assert_eq!(
            block_on(head_state(&meta)),
            Err(crate::testing::MemError::UnknownWorktree)
        );

        meta.with_head(Head::Branch(main.clone()));
        assert_eq!(
            block_on(head_state(&meta)),
            Ok(HeadState::Unborn {
                branch: main.clone()
            })
        );

        let root = block_on(meta.create_commit(100, &[])).unwrap();
        block_on(meta.create_ref(&Ref::branch(main.clone()), root, 100)).unwrap();
        assert_eq!(
            block_on(head_state(&meta)),
            Ok(HeadState::Attached {
                branch: main,
                commit: root
            })
        );

        let detached = CommitId([7; 32]);
        meta.with_head(Head::Detached(detached));
        assert_eq!(
            block_on(head_state(&meta)),
            Ok(HeadState::Detached { commit: detached })
        );
    }
}
