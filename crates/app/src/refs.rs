//! Ref objects: the store's named, movable pointers to commits.
//!
//! A ref names a commit and moves with the history it names. Exactly one
//! ref is active: the one new commits hang from. This layer only reads
//! refs: commits move the active ref through the store, and no command
//! creates, deletes, or activates a ref.

use sekai_core::{CommitId, CommitRef};
use sekai_storage::SekaiStore;

use crate::error::AppError;
use crate::instance::SekaiInstance;

impl<S: SekaiStore> SekaiInstance<S> {
    /// The active ref, or `None` when the store has none.
    pub async fn active_ref(&self) -> Result<Option<CommitRef>, AppError> {
        sekai_storage::refs::active_ref(self.store.meta())
            .await
            .map_err(AppError::Commit)
    }

    /// The commit the next one would continue: the active ref's target.
    ///
    /// `None` when the active ref is unborn. The next commit then records
    /// with no parents, starting a fresh root even if the store already
    /// holds commits.
    ///
    /// This is not [`SekaiInstance::latest_commit_id`], which reports the
    /// newest stored commit: the active ref's target need not be the
    /// newest commit.
    ///
    /// Any caller may run it, including one holding a world lock.
    pub async fn current_commit_id(&self) -> Result<Option<CommitId>, AppError> {
        Ok(sekai_storage::refs::active_ref(self.store.meta())
            .await
            .map_err(AppError::Commit)?
            .and_then(|active| active.commit))
    }
}
