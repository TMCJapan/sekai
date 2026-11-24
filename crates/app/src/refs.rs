//! Ref objects: the store's named, movable pointers to commits.
//!
//! A ref names a commit and moves with the history it names. New commits
//! land on the HEAD of the worktree the store was opened from. This layer
//! only reads refs: commits move HEAD's branch through the store, and no
//! command creates or deletes a ref.

use alloc::vec::Vec;

use sekai_core::{CommitId, Head, HeadState, RefKind, RefName};
use sekai_storage::SekaiStore;
use sekai_util::{Clock, ThreadRunner};

use crate::error::AppError;
use crate::instance::SekaiInstance;

/// A branch as [`SekaiInstance::list_branches`] lists it from the opened
/// worktree: `head` depends on where the store was opened, and the unborn
/// branch HEAD names is listed with no ref behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedBranch {
    /// The branch.
    pub name: RefName,
    /// Its commit; `None` for the unborn branch HEAD names.
    pub commit: Option<CommitId>,
    /// Whether the opened worktree's HEAD names it.
    pub head: bool,
}

impl<S: SekaiStore, T: ThreadRunner, B: Clock> SekaiInstance<S, T, B> {
    /// The opened worktree's HEAD, resolved against the refs.
    pub async fn head(&self) -> Result<HeadState, AppError> {
        Ok(sekai_storage::refs::head_state(self.store.meta()).await?)
    }

    /// `head`, any worktree's, resolved against the refs. Unlike
    /// [`Self::head`], it works from any location the store was opened at.
    pub async fn resolve_head(&self, head: Head) -> Result<HeadState, AppError> {
        Ok(sekai_storage::refs::resolve_head(self.store.meta(), head).await?)
    }

    /// Every branch in full refname order, HEAD's flagged.
    ///
    /// The unborn branch HEAD names is listed too, with no commit, so a
    /// fresh store lists `main` (deviation: Git lists only refs, and an
    /// unborn branch has none).
    pub async fn list_branches(&self) -> Result<Vec<ListedBranch>, AppError> {
        let head = self.head().await?;
        let refs = sekai_storage::refs::list_refs_of(self.store.meta(), RefKind::Branch)
            .await
            .map_err(AppError::Commit)?;
        let mut branches: Vec<ListedBranch> = refs
            .into_iter()
            .map(|record| ListedBranch {
                head: head.branch() == Some(&record.name.name),
                name: record.name.name,
                commit: record.commit,
            })
            .collect();
        // Within one ref kind, short names sort as their full refnames do.
        if let HeadState::Unborn { branch } = head
            && let Err(at) = branches.binary_search_by(|listed| listed.name.cmp(&branch))
        {
            branches.insert(
                at,
                ListedBranch {
                    name: branch,
                    commit: None,
                    head: true,
                },
            );
        }
        Ok(branches)
    }

    /// The commit the next one would continue: HEAD's commit.
    ///
    /// `None` when HEAD is unborn. The next commit then records with no
    /// parents, starting a fresh root even if the store already holds
    /// commits. Fails when the store was opened from no worktree.
    ///
    /// This is not [`SekaiInstance::latest_commit_id`], which reports the
    /// newest stored commit: HEAD's commit need not be the newest.
    ///
    /// Any caller may run it, including one holding a world lock.
    pub async fn current_commit_id(&self) -> Result<Option<CommitId>, AppError> {
        Ok(self.head().await?.commit())
    }
}
