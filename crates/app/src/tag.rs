//! Commit tags: human-readable aliases (`@before-update`).
//!
//! Tags are metadata only: they point at commits, constrain nothing,
//! and die with their commit (pruning cascades). Creation
//! check-then-inserts so duplicates fail loudly instead of surfacing
//! backend constraint errors.

use std::time::{SystemTime, UNIX_EPOCH};

use sekai_core::{CommitId, CommitTag, MetaStore as _, TagName};
use sekai_storage::{SekaiStore, StoreLockMode};

use crate::error::AppError;
use crate::instance::SekaiInstance;

impl<S: SekaiStore> SekaiInstance<S> {
    /// Point `name` at `commit`, returning the created record.
    ///
    /// An existing name fails with [`AppError::TagExists`] unless `force`
    /// moves it.
    pub async fn create_tag(
        &mut self,
        name: &TagName,
        commit: CommitId,
        force: bool,
    ) -> Result<CommitTag, AppError> {
        // The lock only keeps `prune` out. Lookup, untag, and insert are
        // separate statements, so concurrent edits of one tag can
        // interleave; that needs a transaction, not a stronger lock, and
        // is tracked separately.
        let _lock = self.lock_store(StoreLockMode::Shared)?;
        if self.store.meta().lookup_commit(commit).await?.is_none() {
            return Err(AppError::CommitRef(
                sekai_core::ResolveError::UnknownCommit { id: commit.0 },
            ));
        }
        if self.store.meta().lookup_tag(name).await?.is_some() {
            if !force {
                return Err(AppError::TagExists {
                    name: name.as_str().to_owned(),
                });
            }
            self.store.meta_mut().untag(name).await?;
        }
        let created_at_ms =
            u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())
                .unwrap_or(u64::MAX);
        self.store
            .meta_mut()
            .tag_commit(name, commit, created_at_ms)
            .await?;
        Ok(CommitTag::new(name.clone(), commit, created_at_ms))
    }

    /// Remove `name`, returning whether a tag was removed.
    pub async fn delete_tag(&mut self, name: &TagName) -> Result<bool, AppError> {
        let _lock = self.lock_store(StoreLockMode::Shared)?;
        Ok(self.store.meta_mut().untag(name).await?)
    }

    /// List tags in name order.
    pub async fn list_tags(&self) -> Result<Vec<CommitTag>, AppError> {
        let _lock = self.lock_store(StoreLockMode::Shared)?;
        sekai_core::usecase::graph::list_tags(self.store.meta())
            .await
            .map_err(AppError::Commit)
    }

    /// Resolve a commit reference (`<id>` or `@tag`).
    pub async fn resolve_commit_ref(&self, raw: &str) -> Result<CommitId, AppError> {
        let _lock = self.lock_store(StoreLockMode::Shared)?;
        sekai_core::usecase::graph::resolve_commit_ref(self.store.meta(), raw)
            .await
            .map_err(AppError::CommitRef)
    }

    /// Count fresh and effective rows for one commit.
    pub async fn commit_stats(
        &self,
        commit: CommitId,
    ) -> Result<sekai_core::CommitStats, AppError> {
        let _lock = self.lock_store(StoreLockMode::Shared)?;
        sekai_core::usecase::graph::commit_stats(self.store.meta(), commit)
            .await
            .map_err(AppError::Commit)
    }
}
