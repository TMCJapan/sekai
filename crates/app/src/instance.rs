//! Opened store instance and world handles.
//!
//! [`SekaiInstance`] is the single entry point for store-scoped operations:
//! the store is opened once per process instead of once per free function,
//! and metadata/CAS adapters are reached through one place. Operations that
//! touch a world hang off borrowed handles: [`WorldHandle`] for read-only
//! work, [`WorldHandleMut`] for runs that write, so write exclusivity shows
//! in the type.

use std::path::{Path, PathBuf};

use sekai_core::{Snapshot, SnapshotId};

use crate::error::AppError;

/// An opened store; all store-scoped operations go through this.
#[derive(Debug, Clone)]
pub struct SekaiInstance {
    pub(crate) store: sekai_storage::SqliteStore,
}

impl SekaiInstance {
    /// Open the store named by `store_url`.
    ///
    /// `sqlite://<dir>` (or a bare `<dir>`) opens a SQLite store; anything
    /// else fails loudly, including backends not compiled in.
    pub async fn open(store_url: &str) -> Result<Self, AppError> {
        let (kind, rest) = sekai_storage::parse_backend_url(store_url)?;
        // `Sqlite` is currently the only variant; the pattern becomes
        // refutable once stub backends land, and anything else fails below.
        #[cfg(feature = "backend-sqlite")]
        if kind == sekai_storage::BackendKind::Sqlite {
            return Ok(Self {
                store: sekai_storage::open_sqlite(Path::new(rest)).await?,
            });
        }
        Err(sekai_storage::StorageError::UnsupportedBackend {
            url: store_url.to_owned(),
        }
        .into())
    }

    /// Store handle (metadata plus CAS) for escape-hatch access.
    pub const fn store(&self) -> &sekai_storage::SqliteStore {
        &self.store
    }

    /// Mutable store handle for escape-hatch access.
    pub const fn store_mut(&mut self) -> &mut sekai_storage::SqliteStore {
        &mut self.store
    }

    /// Read-only handle rooted at `root`.
    pub fn world(&self, root: impl AsRef<Path>) -> WorldHandle<'_> {
        WorldHandle {
            instance: self,
            root: root.as_ref().to_path_buf(),
        }
    }

    /// Read-write handle rooted at `root`.
    pub fn world_mut(&mut self, root: impl AsRef<Path>) -> WorldHandleMut<'_> {
        WorldHandleMut {
            instance: self,
            root: root.as_ref().to_path_buf(),
        }
    }

    /// List all snapshots in ID order (for `list` and pre-flight checks).
    pub async fn list_snapshots(&self) -> Result<Vec<Snapshot>, AppError> {
        sekai_core::usecase::snapshot::list_snapshots(self.store.meta())
            .await
            .map_err(AppError::Snapshot)
    }

    /// Latest snapshot ID; fails when the store has no snapshots.
    pub async fn latest_snapshot_id(&self) -> Result<SnapshotId, AppError> {
        sekai_core::usecase::snapshot::latest_snapshot_id(self.store.meta())
            .await
            .map_err(AppError::Snapshot)?
            .ok_or(AppError::NoSnapshots)
    }
}

/// Read-only handle to one world directory bound to an instance.
pub struct WorldHandle<'a> {
    pub(crate) instance: &'a SekaiInstance,
    pub(crate) root: PathBuf,
}

impl WorldHandle<'_> {
    /// World root this handle is bound to.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Read-write handle to one world directory bound to an instance.
///
/// Writes go through `&mut self`, and the handle borrows the instance
/// mutably: while a write session is alive, store-scoped operations on the
/// same instance cannot start.
pub struct WorldHandleMut<'a> {
    pub(crate) instance: &'a mut SekaiInstance,
    pub(crate) root: PathBuf,
}

impl WorldHandleMut<'_> {
    /// World root this handle is bound to.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Read-only view of the same world, for read operations issued while a
    /// write session is held.
    pub(crate) fn view(&self) -> WorldHandle<'_> {
        WorldHandle {
            instance: &*self.instance,
            root: self.root.clone(),
        }
    }
}
