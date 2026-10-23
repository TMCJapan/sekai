//! Ref objects: the store's named, movable pointers to commits.
//!
//! A ref names a commit and moves with the history it names. Exactly one
//! ref is active: the one new commits hang from, and the only ref
//! `delete_ref` refuses to remove. Refs carry no content of their own -
//! they pin no commit and change nothing a checkout sees - and a retired
//! commit hands its ref to the commit that folded it (pruning). Changing
//! the active ref decides where the next commit lands, so it runs under
//! the writer lock.

use sekai_core::{CommitId, CommitRef, MetaStore as _, RefName};
use sekai_storage::{SekaiStore, StorageError, StoreLockMode};

use crate::error::AppError;
use crate::instance::SekaiInstance;
use crate::time::now_ms;

impl<S: SekaiStore> SekaiInstance<S> {
    /// Point `name` at `commit`, creating the ref when it does not exist.
    ///
    /// `commit = None` leaves the ref unborn: it names a history without
    /// a commit, while `Some(id)` must name a stored commit. An existing
    /// name fails with [`AppError::RefExists`] unless `force` moves it; a
    /// move keeps the ref's active flag and creation time.
    ///
    /// `force` is a policy of this layer: the port's `set_ref` always
    /// moves the ref.
    ///
    /// Every read runs under the writer lock, which excludes the app
    /// operations that change commits or refs (`commit`, `prune`, and the
    /// other ref mutations): a conflicting operation fails fast with
    /// `StoreLocked` instead of running concurrently. A staged target
    /// cannot vanish between the check and the write. Handles using the
    /// backend directly are outside this protocol.
    pub async fn create_ref(
        &mut self,
        name: &RefName,
        commit: Option<CommitId>,
        force: bool,
    ) -> Result<CommitRef, AppError> {
        let _lock = self.lock_store(StoreLockMode::Writer)?;
        if let Some(commit) = commit
            && self.store.meta().lookup_commit(commit).await?.is_none()
        {
            return Err(AppError::CommitRef(
                sekai_core::ResolveError::UnknownCommit { id: commit.raw() },
            ));
        }
        let existing = self.store.meta().lookup_ref(name).await?;
        if existing.is_some() && !force {
            return Err(AppError::RefExists {
                name: name.as_str().to_owned(),
            });
        }
        // The port's contract fixes what is stored: a new ref starts
        // inactive, and a move keeps the existing flag and creation time.
        let (active, created_at_ms) = match existing {
            Some(record) => (record.active, record.created_at_ms),
            None => (false, now_ms()?),
        };
        self.store
            .meta_mut()
            .set_ref(name, commit, created_at_ms)
            .await
            .map_err(|err| match err {
                StorageError::UnknownCommit { id } => {
                    AppError::CommitRef(sekai_core::ResolveError::UnknownCommit { id })
                }
                other => AppError::Storage(other),
            })?;
        Ok(CommitRef::new(name.clone(), commit, active, created_at_ms))
    }

    /// Remove `name`, returning whether a ref was removed: `false` means
    /// no ref had that name, not that the removal failed.
    ///
    /// The active ref is never removed: a store without one would have
    /// nowhere to hang new commits from. The port is the guard and returns
    /// the protected name, so the app can say which ref to activate
    /// instead.
    pub async fn delete_ref(&mut self, name: &RefName) -> Result<bool, AppError> {
        let _lock = self.lock_store(StoreLockMode::Writer)?;
        self.store
            .meta_mut()
            .delete_ref(name)
            .await
            .map_err(|err| match err {
                StorageError::ActiveRefProtected { name } => AppError::ActiveRef { name },
                other => AppError::Storage(other),
            })
    }

    /// List refs in name order.
    pub async fn list_refs(&self) -> Result<Vec<CommitRef>, AppError> {
        let _lock = self.lock_store(StoreLockMode::Shared)?;
        sekai_core::usecase::refs::list_refs(self.store.meta())
            .await
            .map_err(AppError::Commit)
    }

    /// Make `name` the active ref: the one new commits hang from.
    ///
    /// The port validates both the name and its target, then moves the
    /// flag. The record is read back after activation so the caller sees
    /// what the store actually holds, not a copy staged before the write.
    pub async fn activate_ref(&mut self, name: &RefName) -> Result<CommitRef, AppError> {
        let _lock = self.lock_store(StoreLockMode::Writer)?;
        self.store
            .meta_mut()
            .activate_ref(name)
            .await
            .map_err(|err| match err {
                StorageError::UnknownRef { name } => AppError::UnknownRef { name },
                StorageError::UnknownCommit { id } => {
                    AppError::CommitRef(sekai_core::ResolveError::UnknownCommit { id })
                }
                other => AppError::Storage(other),
            })?;
        self.store
            .meta()
            .lookup_ref(name)
            .await?
            .ok_or_else(|| AppError::UnknownRef {
                name: name.as_str().to_owned(),
            })
    }

    /// The active ref, or `None` when the store has none.
    pub async fn active_ref(&self) -> Result<Option<CommitRef>, AppError> {
        let _lock = self.lock_store(StoreLockMode::Shared)?;
        sekai_core::usecase::refs::active_ref(self.store.meta())
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
    /// newest stored commit: activating an older ref changes the target
    /// here without changing the newest commit.
    ///
    /// Takes a shared store lock. A caller already holding a shared lock
    /// (such as `diff_world_chunks`) may call this; a writer-locked caller
    /// must not nest another acquisition.
    pub async fn current_commit_id(&self) -> Result<Option<CommitId>, AppError> {
        let _lock = self.lock_store(StoreLockMode::Shared)?;
        Ok(sekai_core::usecase::refs::active_ref(self.store.meta())
            .await
            .map_err(AppError::Commit)?
            .and_then(|active| active.commit))
    }
}
