//! Snapshot tags: human-readable aliases (`@before-update`).
//!
//! Tags are metadata only: they point at snapshots, constrain nothing,
//! and die with their snapshot (pruning cascades). Creation
//! check-then-inserts so duplicates fail loudly instead of surfacing
//! backend constraint errors.

use std::time::{SystemTime, UNIX_EPOCH};

use sekai_core::{MetaStore as _, SnapshotId, SnapshotTag, TagName};

use crate::error::AppError;
use crate::instance::SekaiInstance;

impl SekaiInstance {
    /// Point `name` at `snapshot`, returning the created record.
    ///
    /// An existing name fails with [`AppError::TagExists`] unless `force`
    /// moves it.
    pub async fn create_tag(
        &mut self,
        name: &TagName,
        snapshot: SnapshotId,
        force: bool,
    ) -> Result<SnapshotTag, AppError> {
        if self.store.meta().lookup_snapshot(snapshot).await?.is_none() {
            return Err(AppError::SnapshotRef(
                sekai_core::usecase::snapshot::ResolveError::UnknownSnapshot { id: snapshot.0 },
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
            .tag_snapshot(name, snapshot, created_at_ms)
            .await?;
        Ok(SnapshotTag::new(name.clone(), snapshot, created_at_ms))
    }

    /// Remove `name`, returning whether a tag was removed.
    pub async fn delete_tag(&mut self, name: &TagName) -> Result<bool, AppError> {
        Ok(self.store.meta_mut().untag(name).await?)
    }

    /// List tags in name order.
    pub async fn list_tags(&self) -> Result<Vec<SnapshotTag>, AppError> {
        sekai_core::usecase::snapshot::list_tags(self.store.meta())
            .await
            .map_err(AppError::Snapshot)
    }

    /// Resolve a snapshot reference (`<id>` or `@tag`).
    pub async fn resolve_snapshot_ref(&self, raw: &str) -> Result<SnapshotId, AppError> {
        sekai_core::usecase::snapshot::resolve_snapshot_ref(self.store.meta(), raw)
            .await
            .map_err(AppError::SnapshotRef)
    }

    /// Count fresh and effective rows for one snapshot.
    pub async fn snapshot_stats(
        &self,
        snapshot: SnapshotId,
    ) -> Result<sekai_core::SnapshotStats, AppError> {
        sekai_core::usecase::snapshot::snapshot_stats(self.store.meta(), snapshot)
            .await
            .map_err(AppError::Snapshot)
    }
}
