//! Ref and HEAD reads: the store's named, movable pointers to commits.
//!
//! A branch names a commit and moves with the history it names. Each
//! worktree has a HEAD that names a branch or holds a commit directly;
//! every command acts on the main worktree's HEAD. This layer only reads
//! them: commits move HEAD through the store.

use sekai_core::{CommitId, HeadState};
use sekai_storage::{MetaStore as _, SekaiStore};

use crate::error::AppError;
use crate::instance::SekaiInstance;

/// The path of the store's main worktree and its HEAD: the worktree every
/// command acts on.
pub(crate) async fn main_head<S: SekaiStore>(store: &S) -> Result<(String, HeadState), AppError> {
    let path = sekai_storage::refs::main_worktree(store.meta())
        .await
        .map_err(AppError::Commit)?
        .ok_or(AppError::MissingMainWorktree)?;
    let head = store
        .meta()
        .head(&path)
        .await?
        .ok_or(AppError::MissingMainWorktree)?;
    Ok((path, head))
}

impl<S: SekaiStore> SekaiInstance<S> {
    /// The commit the next one would continue: the main worktree's HEAD
    /// commit.
    ///
    /// `None` while HEAD is unborn. The next commit then records with no
    /// parents, starting a fresh root even if the store already holds
    /// commits.
    ///
    /// This is not [`SekaiInstance::latest_commit_id`], which reports the
    /// newest stored commit: HEAD's commit need not be the newest commit.
    ///
    /// Any caller may run it, including one holding a world lock.
    pub async fn current_commit_id(&self) -> Result<Option<CommitId>, AppError> {
        Ok(main_head(&self.store).await?.1.commit())
    }
}
