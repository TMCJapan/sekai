//! Opened store instance and world handles.
//!
//! [`SekaiInstance`] is the single entry point for store-scoped operations:
//! the store is opened once per process instead of once per free function,
//! and metadata/CAS adapters are reached through one place. Operations that
//! touch a world hang off borrowed handles: [`WorldHandle`] for read-only
//! work, [`WorldHandleMut`] for runs that write (backup, rollback), so write
//! exclusivity shows in the type.
//!
//! Exclusion against other processes is per operation: every public
//! operation locks the store (and its world, if any) at entry and releases
//! on return, so an idle instance never blocks anyone. See
//! [`sekai_storage::lock`] for the store matrix and [`WorldTree::lock`] for
//! worlds.

// A build with no compiled backend has nothing left to await in `open`/`init`,
// but their future-returning signatures are part of the API across every
// feature combination.
#![cfg_attr(
    not(feature = "backend-sqlite"),
    allow(clippy::unused_async, clippy::unused_async_trait_impl)
)]

use std::sync::Arc;

use sekai_core::{Snapshot, SnapshotId};
use sekai_storage::{StoreLock, StoreLockMode};
use sekai_world::{WorldLock, WorldLockMode, WorldTree};

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
        #[cfg(feature = "backend-sqlite")]
        {
            let (kind, rest) = sekai_storage::parse_backend_url(store_url)?;
            // `Sqlite` is currently the only variant; the pattern becomes
            // refutable once stub backends land, and anything else fails below.
            if kind == sekai_storage::BackendKind::Sqlite {
                return Ok(Self {
                    store: sekai_storage::open_sqlite(rest).await?,
                });
            }
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
        #[cfg(feature = "backend-sqlite")]
        {
            let (kind, rest) = sekai_storage::parse_backend_url(store_url)?;
            if kind == sekai_storage::BackendKind::Sqlite {
                return Ok(Self {
                    store: sekai_storage::init_sqlite(rest).await?,
                });
            }
        }
        Err(sekai_storage::StorageError::UnsupportedBackend {
            url: store_url.to_owned(),
        }
        .into())
    }

    /// Store handle (metadata plus CAS) for escape-hatch access. Access
    /// through it takes no lock; callers coordinate with other processes
    /// themselves.
    pub const fn store(&self) -> &sekai_storage::SqliteStore {
        &self.store
    }

    /// Mutable store handle for escape-hatch access; takes no lock, like
    /// [`Self::store`].
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

    pub(crate) fn lock_store(&self, mode: StoreLockMode) -> Result<StoreLock, AppError> {
        Ok(self.store.cas().lock(mode)?)
    }

    /// List all snapshots in ID order (for `list` and pre-flight checks).
    pub async fn list_snapshots(&self) -> Result<Vec<Snapshot>, AppError> {
        let _lock = self.lock_store(StoreLockMode::Shared)?;
        sekai_core::usecase::snapshot::list_snapshots(self.store.meta())
            .await
            .map_err(AppError::Snapshot)
    }

    /// Latest snapshot ID; fails when the store has no snapshots.
    pub async fn latest_snapshot_id(&self) -> Result<SnapshotId, AppError> {
        let lock = self.lock_store(StoreLockMode::Shared)?;
        self.latest_snapshot_id_locked(&lock).await
    }

    /// [`Self::latest_snapshot_id`] for an operation that already holds
    /// `_held`; asking for it makes the caller prove the lock exists.
    pub(crate) async fn latest_snapshot_id_locked(
        &self,
        _held: &StoreLock,
    ) -> Result<SnapshotId, AppError> {
        sekai_core::usecase::snapshot::latest_snapshot_id(self.store.meta())
            .await
            .map_err(AppError::Snapshot)?
            .ok_or(AppError::NoSnapshots)
    }
}

/// Shared (`Arc`) into the blocking tasks that touch files: a cancelled
/// future cannot abort a blocking task that already started, so the locks
/// must outlive the future until the file pass finishes.
#[derive(Debug)]
pub(crate) struct OperationLocks {
    store: StoreLock,
    _world: Option<WorldLock>,
}

impl OperationLocks {
    pub(crate) fn acquire<T: WorldTree>(
        instance: &SekaiInstance,
        store: StoreLockMode,
        world: &T,
        world_mode: WorldLockMode,
    ) -> Result<Arc<Self>, AppError>
    where
        AppError: From<T::Error>,
    {
        let store = instance.lock_store(store)?;
        let world = world.lock(world_mode)?;
        Ok(Arc::new(Self {
            store,
            _world: world,
        }))
    }

    /// The store lock this operation holds.
    pub(crate) const fn store(&self) -> &StoreLock {
        &self.store
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
