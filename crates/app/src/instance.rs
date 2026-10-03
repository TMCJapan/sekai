//! Opened store instance and world handles.
//!
//! [`SekaiInstance`] is the single entry point for store-scoped operations:
//! the store is opened once per process instead of once per free function,
//! and metadata/CAS adapters are reached through one place. Operations that
//! touch a world hang off borrowed handles: [`WorldHandle`] for read-only
//! work, [`WorldHandleMut`] for runs that write (backup, rollback), so write
//! exclusivity shows in the type.

use sekai_core::{Snapshot, SnapshotId};
use sekai_world::WorldTree;

use crate::error::AppError;

/// An opened store; all store-scoped operations go through this.
pub struct SekaiInstance {
    pub(crate) store: sekai_storage::SqliteStore,
}

impl SekaiInstance {
    /// Open the existing store named by `store_url`.
    ///
    /// `sqlite://<dir>` (or a bare `<dir>`) opens a SQLite store; anything
    /// else fails loudly, including backends not compiled in. A store that
    /// was never initialized fails without creating anything: only
    /// [`Self::init`] materializes a store.
    pub async fn open(store_url: &str) -> Result<Self, AppError> {
        let (kind, rest) = sekai_storage::parse_backend_url(store_url)?;
        // `Sqlite` is currently the only variant; the pattern becomes
        // refutable once stub backends land, and anything else fails below.
        #[cfg(feature = "backend-sqlite")]
        if kind == sekai_storage::BackendKind::Sqlite {
            return Ok(Self {
                store: sekai_storage::open_sqlite(rest).await?,
            });
        }
        Err(sekai_storage::StorageError::UnsupportedBackend {
            url: store_url.to_owned(),
        }
        .into())
    }

    /// Open the store named by `store_url`, initializing it when missing.
    ///
    /// This is the creation path reserved for `backup`; every other command
    /// must go through [`Self::open`] so a missing store fails loudly instead
    /// of leaving an empty store behind.
    pub async fn init(store_url: &str) -> Result<Self, AppError> {
        let (kind, rest) = sekai_storage::parse_backend_url(store_url)?;
        #[cfg(feature = "backend-sqlite")]
        if kind == sekai_storage::BackendKind::Sqlite {
            return Ok(Self {
                store: sekai_storage::init_sqlite(rest).await?,
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
    pub const fn world<'a, T: WorldTree>(&'a self, world: &'a T) -> WorldHandle<'a, T> {
        WorldHandle {
            instance: self,
            world,
        }
    }

    /// Read-write handle rooted at `root`.
    pub const fn world_mut<'a, T: WorldTree>(
        &'a mut self,
        world: &'a mut T,
    ) -> WorldHandleMut<'a, T> {
        WorldHandleMut {
            instance: self,
            world,
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
pub struct WorldHandle<'a, T> {
    pub(crate) instance: &'a SekaiInstance,
    pub(crate) world: &'a T,
}

/// Read-write handle to one world directory bound to an instance.
///
/// Runs that write (`backup`, `rollback`) go through `&mut self`, and the
/// handle borrows the instance mutably: while a write session is alive, no
/// second handle for the same instance can exist, so store-scoped
/// operations and other world writes cannot start.
pub struct WorldHandleMut<'a, T> {
    pub(crate) instance: &'a mut SekaiInstance,
    pub(crate) world: &'a mut T,
}

impl<T> WorldHandleMut<'_, T> {
    /// Read-only view of the same world, for read operations issued while a
    /// write session is held.
    pub(crate) const fn view(&self) -> WorldHandle<'_, T> {
        WorldHandle {
            instance: &*self.instance,
            world: &*self.world,
        }
    }
}
