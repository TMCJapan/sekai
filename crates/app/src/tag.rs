//! Snapshot tags: human-readable aliases (`@before-update`).
//!
//! Tags are metadata only: they point at snapshots, constrain nothing,
//! and die with their snapshot (pruning cascades). Creation
//! check-then-inserts so duplicates fail loudly instead of surfacing
//! backend constraint errors.

use std::time::{SystemTime, UNIX_EPOCH};

use sekai_core::{MetaStore as _, SnapshotId, SnapshotTag, TagName};

use crate::error::AppError;

/// Point `name` at `snapshot`, returning the created record.
///
/// An existing name fails with [`AppError::TagExists`] unless `force`
/// moves it.
pub async fn create_tag(
    store_url: &str,
    name: &TagName,
    snapshot: SnapshotId,
    force: bool,
) -> Result<SnapshotTag, AppError> {
    let mut store = super::open_store(store_url).await?;
    if store.meta().lookup_snapshot(snapshot).await?.is_none() {
        return Err(AppError::SnapshotRef(
            sekai_core::usecase::snapshot::ResolveError::UnknownSnapshot { id: snapshot.0 },
        ));
    }
    if store.meta().lookup_tag(name).await?.is_some() {
        if !force {
            return Err(AppError::TagExists {
                name: name.as_str().to_owned(),
            });
        }
        store.meta_mut().untag(name).await?;
    }
    let created_at_ms = u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())
        .unwrap_or(u64::MAX);
    store
        .meta_mut()
        .tag_snapshot(name, snapshot, created_at_ms)
        .await?;
    Ok(SnapshotTag::new(name.clone(), snapshot, created_at_ms))
}

/// Remove `name`, returning whether a tag was removed.
pub async fn delete_tag(store_url: &str, name: &TagName) -> Result<bool, AppError> {
    let mut store = super::open_store(store_url).await?;
    Ok(store.meta_mut().untag(name).await?)
}

/// List tags in name order.
pub async fn list_tags(store_url: &str) -> Result<Vec<SnapshotTag>, AppError> {
    let store = super::open_store(store_url).await?;
    sekai_core::usecase::snapshot::list_tags(store.meta())
        .await
        .map_err(AppError::Snapshot)
}

/// Resolve a snapshot reference (`<id>` or `@tag`).
pub async fn resolve_snapshot_ref(store_url: &str, raw: &str) -> Result<SnapshotId, AppError> {
    let store = super::open_store(store_url).await?;
    sekai_core::usecase::snapshot::resolve_snapshot_ref(store.meta(), raw)
        .await
        .map_err(AppError::SnapshotRef)
}
