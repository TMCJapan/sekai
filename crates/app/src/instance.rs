//! Opened store instance and world handles.
//!
//! [`SekaiInstance`] is the single entry point for store-scoped operations:
//! the store is opened once per process instead of once per free function,
//! and metadata/CAS adapters are reached through one place. The store type
//! is pluggable: any [`SekaiStore`] backend can be used, and the SQLite
//! backend additionally gets the convenience `open`/`init` entry points.
//! Operations that touch a world hang off borrowed handles: [`WorldHandle`]
//! for read-only work, [`WorldHandleMut`] for runs that write (commit,
//! checkout), so write exclusivity shows in the type.
//!
//! Exclusion against other processes is per operation: every public
//! operation locks the store (and its world, if any) at entry and releases
//! on return, so an idle instance never blocks anyone. See
//! [`sekai_storage::lock`] for the store matrix and [`WorldTree::lock`] for
//! worlds.

use std::sync::Arc;

use sekai_core::{Commit, CommitId};
use sekai_storage::{SekaiStore, SekaiStoreOpener, StoreLockMode};
use sekai_world::{WorldLock, WorldLockMode, WorldLockState, WorldTree};

use crate::error::AppError;

/// An opened store; all store-scoped operations go through this.
pub struct SekaiInstance<T: SekaiStore> {
    pub(crate) store: T,
}

impl<T: SekaiStore> SekaiInstance<T> {
    /// Compose an instance from an already opened store.
    pub const fn new(store: T) -> Self {
        Self { store }
    }

    /// Store handle (metadata plus CAS) for escape-hatch access. Access
    /// through it takes no lock; callers coordinate with other processes
    /// themselves.
    pub const fn store(&self) -> &T {
        &self.store
    }

    /// Mutable store handle for escape-hatch access; takes no lock, like
    /// [`Self::store`].
    pub const fn store_mut(&mut self) -> &mut T {
        &mut self.store
    }

    /// Read-only handle rooted at `root`.
    pub const fn world<'a, W: WorldTree>(&'a self, world: &'a W) -> WorldHandle<'a, W, T> {
        WorldHandle {
            instance: self,
            world,
        }
    }

    /// Read-write handle rooted at `root`.
    pub const fn world_mut<'a, W: WorldTree>(
        &'a mut self,
        world: &'a mut W,
    ) -> WorldHandleMut<'a, W, T> {
        WorldHandleMut {
            instance: self,
            world,
        }
    }

    pub(crate) fn lock_store(&self, mode: StoreLockMode) -> Result<T::LockGuard, AppError> {
        Ok(self.store.lock(mode)?)
    }

    /// List all commits in local record order (for `log` and pre-flight checks).
    pub async fn list_commits(&self) -> Result<Vec<Commit>, AppError> {
        let _lock = self.lock_store(StoreLockMode::Shared)?;
        sekai_core::usecase::graph::list_commits(self.store.meta())
            .await
            .map_err(AppError::Commit)
    }

    /// The commits a commit continues: its direct parents.
    ///
    /// Fails when the commit does not exist.
    pub async fn commit_parents(&self, commit: CommitId) -> Result<Vec<CommitId>, AppError> {
        let _lock = self.lock_store(StoreLockMode::Shared)?;
        sekai_core::usecase::graph::commit_parents(self.store.meta(), commit)
            .await
            .map_err(AppError::Commit)
    }

    /// Every ancestor of a commit, nearest first.
    ///
    /// Fails when the commit does not exist.
    pub async fn commit_ancestors(&self, commit: CommitId) -> Result<Vec<CommitId>, AppError> {
        let _lock = self.lock_store(StoreLockMode::Shared)?;
        sekai_core::usecase::graph::commit_ancestors(self.store.meta(), commit)
            .await
            .map_err(AppError::Commit)
    }

    /// The history of a commit: the commit itself followed by its
    /// ancestors, nearest first.
    ///
    /// Fails when the commit does not exist.
    pub async fn commit_history(&self, commit: CommitId) -> Result<Vec<Commit>, AppError> {
        let _lock = self.lock_store(StoreLockMode::Shared)?;
        sekai_core::usecase::graph::commit_history(self.store.meta(), commit)
            .await
            .map_err(AppError::Commit)
    }

    /// Latest commit ID; fails when the store has no commits.
    ///
    /// This is the newest commit of the store, not the one the next
    /// commit would continue - see [`SekaiInstance::current_commit_id`].
    pub async fn latest_commit_id(&self) -> Result<CommitId, AppError> {
        let _lock = self.lock_store(StoreLockMode::Shared)?;
        sekai_core::usecase::graph::latest_commit_id(self.store.meta())
            .await
            .map_err(AppError::Commit)?
            .ok_or(AppError::NoCommits)
    }

    /// Resolve a commit reference (`<64-char hash>` or a ref name).
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

#[cfg(feature = "backend-sqlite")]
impl SekaiInstance<sekai_storage_sqlite::SqliteStore> {
    /// Open the existing store named by `store_url`.
    ///
    /// `sqlite://<dir>` (or a bare `<dir>`) opens a SQLite store; anything
    /// else fails loudly, including backends not compiled in. A store that
    /// was never initialized fails without creating anything: only
    /// [`Self::init`] materializes a store.
    pub async fn open(store_url: &str) -> Result<Self, AppError> {
        Ok(Self {
            store: sekai_storage_sqlite::open_sqlite(store_url).await?,
        })
    }

    /// Open the store named by `store_url`, initializing it when missing.
    ///
    /// This is the creation path reserved for `commit`; every other command
    /// must go through [`Self::open`] so a missing store fails loudly instead
    /// of leaving an empty store behind.
    pub async fn init(store_url: &str) -> Result<Self, AppError> {
        Ok(Self {
            store: sekai_storage_sqlite::init_sqlite(store_url).await?,
        })
    }
}

impl<T: SekaiStore + SekaiStoreOpener> SekaiInstance<T> {
    /// Open the existing store of an explicitly chosen backend type.
    pub async fn open_with(store_url: &str) -> Result<Self, AppError> {
        Ok(Self {
            store: T::open(store_url).await?,
        })
    }

    /// Open the store of an explicitly chosen backend type, creating it
    /// when missing.
    pub async fn init_with(store_url: &str) -> Result<Self, AppError> {
        Ok(Self {
            store: T::init(store_url).await?,
        })
    }
}

/// Shared (`Arc`) into the blocking tasks that touch files: a cancelled
/// future cannot abort a blocking task that already started, so the locks
/// must outlive the future until the file pass finishes.
#[derive(Debug)]
pub(crate) struct OperationLocks<S: SekaiStore, L> {
    /// Held for its drop: the lock must outlive the operation.
    _store: S::LockGuard,
    _world: WorldLockState<L>,
}

impl<S: SekaiStore, L: WorldLock> OperationLocks<S, L> {
    pub(crate) fn acquire<W: WorldTree<Lock = L>>(
        instance: &SekaiInstance<S>,
        store: StoreLockMode,
        world: &W,
        world_mode: WorldLockMode,
    ) -> Result<Arc<Self>, AppError>
    where
        AppError: From<W::Error>,
    {
        let store = instance.lock_store(store)?;
        let world = world.lock(world_mode)?;
        Ok(Arc::new(Self {
            _store: store,
            _world: world,
        }))
    }
}

/// Read-only handle to one world directory bound to an instance.
pub struct WorldHandle<'a, W, S: SekaiStore> {
    pub(crate) instance: &'a SekaiInstance<S>,
    pub(crate) world: &'a W,
}

/// Read-write handle to one world directory bound to an instance.
///
/// Runs that write (`commit`, `checkout`) go through `&mut self`, and the
/// handle borrows the instance mutably: while a write session is alive, no
/// second handle for the same instance can exist, so store-scoped
/// operations and other world writes cannot start.
pub struct WorldHandleMut<'a, W, S: SekaiStore> {
    pub(crate) instance: &'a mut SekaiInstance<S>,
    pub(crate) world: &'a mut W,
}

impl<W, S: SekaiStore> WorldHandleMut<'_, W, S> {
    /// Read-only view of the same world, for read operations issued while a
    /// write session is held.
    pub(crate) const fn view(&self) -> WorldHandle<'_, W, S> {
        WorldHandle {
            instance: &*self.instance,
            world: &*self.world,
        }
    }
}
