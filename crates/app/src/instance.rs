//! Opened store instance and world handles.
//!
//! [`SekaiInstance`] is the single entry point for store-scoped operations:
//! the store is opened once per process instead of once per free function,
//! and metadata/CAS adapters are reached through one place. The store type
//! is pluggable: any [`SekaiStore`] backend can be used, and the local
//! backend additionally gets the convenience `open`/`init` entry points.
//! Operations that touch a world hang off borrowed handles: [`WorldHandle`]
//! for read-only work, [`WorldHandleMut`] for runs that write (commit,
//! checkout), so write exclusivity shows in the type.
//!
//! Exclusion is per world, not per operation: blobs are append-only and
//! metadata writes are transactional, so store operations never lock.
//! Opening the store does take the blob keyspace's exclusive session lock,
//! so one instance owns a store at a time and clones share that session. A
//! run that touches a world takes that world's lock at entry and releases
//! it on return; see [`WorldTree::lock`].

use std::sync::{Arc, Mutex};

use sekai_core::{Commit, CommitId};
use sekai_storage::{SekaiStore, SekaiStoreOpener};
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

    /// Store handle (metadata plus CAS) for escape-hatch access: CAS
    /// operations take no lock, and metadata writes go through their own
    /// transactions.
    pub const fn store(&self) -> &T {
        &self.store
    }

    /// Mutable store handle for escape-hatch access; see [`Self::store`].
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

    /// List all commits in local record order (for `log` and pre-flight checks).
    pub async fn list_commits(&self) -> Result<Vec<Commit>, AppError> {
        sekai_core::usecase::graph::list_commits(self.store.meta())
            .await
            .map_err(AppError::Commit)
    }

    /// The commits a commit continues: its direct parents.
    ///
    /// Fails when the commit does not exist.
    pub async fn commit_parents(&self, commit: CommitId) -> Result<Vec<CommitId>, AppError> {
        sekai_core::usecase::graph::commit_parents(self.store.meta(), commit)
            .await
            .map_err(AppError::Commit)
    }

    /// Every ancestor of a commit, nearest first.
    ///
    /// Fails when the commit does not exist.
    pub async fn commit_ancestors(&self, commit: CommitId) -> Result<Vec<CommitId>, AppError> {
        sekai_core::usecase::graph::commit_ancestors(self.store.meta(), commit)
            .await
            .map_err(AppError::Commit)
    }

    /// The history of a commit: the commit itself followed by its
    /// ancestors, nearest first.
    ///
    /// Fails when the commit does not exist.
    pub async fn commit_history(&self, commit: CommitId) -> Result<Vec<Commit>, AppError> {
        sekai_core::usecase::graph::commit_history(self.store.meta(), commit)
            .await
            .map_err(AppError::Commit)
    }

    /// Latest commit ID; fails when the store has no commits.
    ///
    /// This is the newest commit of the store, not the one the next
    /// commit would continue - see [`SekaiInstance::current_commit_id`].
    pub async fn latest_commit_id(&self) -> Result<CommitId, AppError> {
        sekai_core::usecase::graph::latest_commit_id(self.store.meta())
            .await
            .map_err(AppError::Commit)?
            .ok_or(AppError::NoCommits)
    }

    /// Resolve a commit reference (`<64-char hash>` or a ref name).
    pub async fn resolve_commit_ref(&self, raw: &str) -> Result<CommitId, AppError> {
        sekai_core::usecase::graph::resolve_commit_ref(self.store.meta(), raw)
            .await
            .map_err(AppError::CommitRef)
    }

    /// Count fresh and effective rows for one commit.
    pub async fn commit_stats(
        &self,
        commit: CommitId,
    ) -> Result<sekai_core::CommitStats, AppError> {
        sekai_core::usecase::graph::commit_stats(self.store.meta(), commit)
            .await
            .map_err(AppError::Commit)
    }
}

#[cfg(feature = "backend-local")]
impl SekaiInstance<sekai_storage_local::LocalStore> {
    /// Open the existing store rooted at `root`.
    ///
    /// A store that was never initialized fails without creating anything:
    /// only [`Self::init`] materializes a store.
    pub async fn open(root: &str) -> Result<Self, AppError> {
        Ok(Self {
            store: sekai_storage_local::open_local(root).await?,
        })
    }

    /// Initialize the store rooted at `root` and open it.
    ///
    /// This is the creation path reserved for an explicit init (the CLI's
    /// `sekai init`); every other command must go through [`Self::open`] so
    /// a missing store fails loudly instead of leaving an empty store
    /// behind. Initializing an existing store again is idempotent.
    pub async fn init(root: &str) -> Result<Self, AppError> {
        Ok(Self {
            store: sekai_storage_local::init_local(root).await?,
        })
    }
}

impl<T: SekaiStore + SekaiStoreOpener> SekaiInstance<T> {
    /// Open the existing store of an explicitly chosen backend type.
    pub async fn open_with(location: &str) -> Result<Self, AppError> {
        Ok(Self {
            store: T::open(location).await?,
        })
    }

    /// Initialize the store of an explicitly chosen backend type and open
    /// it; the only entry point that materializes a store. Initializing an
    /// existing store again is idempotent.
    pub async fn init_with(location: &str) -> Result<Self, AppError> {
        Ok(Self {
            store: T::init(location).await?,
        })
    }
}

/// A world lock shared (`Arc`) into the blocking tasks that touch files.
///
/// Read-only passes never write, so it is safe for the future to drop the
/// lock's last reference while a reader finishes: the task simply holds a
/// `WorldLockState` value with nowhere left to put it.
#[derive(Debug)]
pub(crate) struct SharedWorldGuard<L> {
    /// Held for its drop: the lock must outlive the operation.
    _world: WorldLockState<L>,
}

impl<L: WorldLock> SharedWorldGuard<L> {
    pub(crate) fn acquire<W: WorldTree<Lock = L>>(
        world: &W,
        mode: WorldLockMode,
    ) -> Result<Arc<Self>, AppError>
    where
        AppError: From<W::Error>,
    {
        Ok(Arc::new(Self {
            _world: world.lock(mode)?,
        }))
    }
}

/// A world lock whose release the lock owner itself decides.
///
/// The guard is shared (`Arc`) with the blocking tasks that touch files, so
/// the lock outlives the future that started them even when that future is
/// cancelled. Ownership sits behind a [`Mutex`] so a task can *take* it: the
/// last task to finish the operation puts it back, and dropping the guard -
/// the last `Arc` - is what finally releases the world.
#[derive(Debug)]
pub(crate) struct WorldGuard<L> {
    /// Taken by the task that ends the operation, which gives it back.
    state: Mutex<Option<WorldLockState<L>>>,
}

impl<L> WorldGuard<L> {
    /// Drop the held lock, if any. Idempotent: later calls do nothing.
    pub(crate) fn release(&self) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        drop(state);
    }
}

impl<L: WorldLock> WorldGuard<L> {
    pub(crate) fn acquire<W: WorldTree<Lock = L>>(
        world: &W,
        mode: WorldLockMode,
    ) -> Result<Arc<Self>, AppError>
    where
        AppError: From<W::Error>,
    {
        Ok(Arc::new(Self {
            state: Mutex::new(Some(world.lock(mode)?)),
        }))
    }
}

/// The world held exclusively by a blocking file pass.
///
/// A cancelled future cannot abort a blocking task that already started, so
/// the lock travels *inside* that task: released only when the task really
/// ends, never when the future's async half is dropped. The store needs no
/// such guard - operations never lock it.
#[derive(Debug)]
pub(crate) struct ExclusiveWorldGuard<L> {
    /// Shared with every queued step; the operation ends by releasing it.
    inner: Arc<WorldGuard<L>>,
}

impl<L: WorldLock> ExclusiveWorldGuard<L> {
    pub(crate) fn acquire<W: WorldTree<Lock = L>>(
        world: &W,
        mode: WorldLockMode,
    ) -> Result<Self, AppError>
    where
        AppError: From<W::Error>,
    {
        Ok(Self {
            inner: WorldGuard::acquire(world, mode)?,
        })
    }

    /// The shared handle every step holds a clone of.
    pub(crate) fn into_shared(self) -> Arc<WorldGuard<L>> {
        self.inner
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
