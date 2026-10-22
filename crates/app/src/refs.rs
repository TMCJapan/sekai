//! Ref objects: the store's named, movable pointers to commits.
//!
//! A ref names a commit and moves with the history it names. Exactly one
//! ref is active - the one new commits hang from. Refs are metadata
//! only: they constrain nothing, and a retired commit hands its ref to
//! the commit that folded it (pruning). Changing the active ref decides
//! where the next commit lands, so it runs under the writer lock.

use std::time::{SystemTime, UNIX_EPOCH};

use sekai_core::{CommitId, CommitRef, MetaStore as _, RefName};
use sekai_storage::{SekaiStore, StoreLockMode};

use crate::error::AppError;
use crate::instance::SekaiInstance;

impl<S: SekaiStore> SekaiInstance<S> {
    /// Point `name` at `commit`, creating the ref when it does not exist.
    ///
    /// `commit = None` leaves the ref unborn: it names a history without
    /// a commit. An existing name fails with [`AppError::RefExists`]
    /// unless `force` moves it.
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
        if self.store.meta().lookup_ref(name).await?.is_some() && !force {
            return Err(AppError::RefExists {
                name: name.as_str().to_owned(),
            });
        }
        let created_at_ms = now_ms()?;
        self.store
            .meta_mut()
            .set_ref(name, commit, created_at_ms)
            .await?;
        self.stored_ref(name).await
    }

    /// Remove `name`, returning whether a ref was removed.
    ///
    /// The active ref is never removed: a store without one would have
    /// nowhere to hang new commits from.
    pub async fn delete_ref(&mut self, name: &RefName) -> Result<bool, AppError> {
        let _lock = self.lock_store(StoreLockMode::Writer)?;
        if self
            .store
            .meta()
            .active_ref()
            .await?
            .is_some_and(|active| active.name == *name)
        {
            return Err(AppError::ActiveRef {
                name: name.as_str().to_owned(),
            });
        }
        Ok(self.store.meta_mut().delete_ref(name).await?)
    }

    /// List refs in name order.
    pub async fn list_refs(&self) -> Result<Vec<CommitRef>, AppError> {
        let _lock = self.lock_store(StoreLockMode::Shared)?;
        sekai_core::usecase::refs::list_refs(self.store.meta())
            .await
            .map_err(AppError::Commit)
    }

    /// Make `name` the active ref: the one new commits hang from.
    pub async fn activate_ref(&mut self, name: &RefName) -> Result<CommitRef, AppError> {
        let _lock = self.lock_store(StoreLockMode::Writer)?;
        if self.store.meta().lookup_ref(name).await?.is_none() {
            return Err(AppError::UnknownRef {
                name: name.as_str().to_owned(),
            });
        }
        self.store.meta_mut().activate_ref(name).await?;
        self.stored_ref(name).await
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
    /// `None` while the active ref is unborn - the next commit starts a
    /// history.
    pub async fn current_commit_id(&self) -> Result<Option<CommitId>, AppError> {
        let _lock = self.lock_store(StoreLockMode::Shared)?;
        Ok(self
            .store
            .meta()
            .active_ref()
            .await?
            .and_then(|active| active.commit))
    }

    /// The stored record of a ref, read back after a write.
    async fn stored_ref(&self, name: &RefName) -> Result<CommitRef, AppError> {
        self.store
            .meta()
            .lookup_ref(name)
            .await?
            .ok_or_else(|| AppError::UnknownRef {
                name: name.as_str().to_owned(),
            })
    }
}

fn now_ms() -> Result<u64, AppError> {
    Ok(
        u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())
            .unwrap_or(u64::MAX),
    )
}
