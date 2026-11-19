//! Branches and the HEAD new commits land on (ADR-0035).
//!
//! Commands act on the HEAD of the store's main worktree. Nothing here
//! registers that worktree, so a store without it has no HEAD to commit
//! on.

use sekai_core::{CommitId, CommitRef, HeadState, Ref, RefName, ReflogNote};
use sekai_storage::{MetaStore as _, SekaiStore, StorageError};

use crate::error::AppError;
use crate::instance::SekaiInstance;
use crate::time::now_ms;

/// The world root of the store's main worktree: the worktree every
/// command acts on.
pub(crate) async fn main_worktree<S: SekaiStore>(store: &S) -> Result<String, AppError> {
    let mut path = None;
    store
        .meta()
        .visit_worktrees(|worktree| {
            if worktree.is_main() {
                path = Some(worktree.path.clone());
                return false;
            }
            true
        })
        .await?;
    path.ok_or(AppError::NoMainWorktree)
}

impl<S: SekaiStore> SekaiInstance<S> {
    /// Point branch `name` at `commit`, creating it when it does not
    /// exist.
    ///
    /// An existing branch fails unless `force` moves it.
    pub async fn create_ref(
        &mut self,
        name: &RefName,
        commit: CommitId,
        force: bool,
    ) -> Result<CommitRef, AppError> {
        let branch = Ref::branch(name.clone());
        let at_ms = now_ms()?;
        let meta = self.store.meta_mut();
        let created = format!("branch: Created from {commit}");
        match meta
            .create_ref(&branch, commit, ReflogNote::new(&created, at_ms))
            .await
        {
            Ok(()) => {}
            Err(StorageError::RefExists { .. }) if force => {
                let reset = format!("branch: Reset to {commit}");
                meta.move_ref(&branch, commit, ReflogNote::new(&reset, at_ms))
                    .await?;
            }
            Err(err) => return Err(err.into()),
        }
        Ok(CommitRef::new(branch, commit))
    }

    /// Remove branch `name`, returning whether a branch was removed:
    /// `false` means no branch had that name, not that the removal failed.
    pub async fn delete_ref(&mut self, name: &RefName) -> Result<bool, AppError> {
        Ok(self
            .store
            .meta_mut()
            .delete_ref(&Ref::branch(name.clone()))
            .await?
            .is_some())
    }

    /// List refs in full refname order.
    pub async fn list_refs(&self) -> Result<Vec<CommitRef>, AppError> {
        sekai_core::usecase::refs::list_refs(self.store.meta())
            .await
            .map_err(AppError::Commit)
    }

    /// The commit HEAD stands at - the one the next commit continues -
    /// or `None` while HEAD is unborn.
    ///
    /// This is not the newest commit in the store: with several branches,
    /// the newest commit may sit on a branch HEAD is not on.
    pub async fn current_commit_id(&self) -> Result<Option<CommitId>, AppError> {
        let main = main_worktree(&self.store).await?;
        Ok(sekai_core::usecase::refs::head(self.store.meta(), &main)
            .await
            .map_err(AppError::Commit)?
            .as_ref()
            .and_then(HeadState::commit))
    }
}
