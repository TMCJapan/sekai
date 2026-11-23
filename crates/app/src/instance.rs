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

use std::sync::Arc;

use sekai_core::{Commit, CommitId, Revision};
use sekai_storage::{CacheStore as _, SekaiStore, SekaiStoreOpener};
use sekai_util::{Sequential, ThreadRunner};
use sekai_world::{WorldError, WorldLock, WorldLockMode, WorldLockState, WorldTree};

use crate::error::AppError;

/// An opened store; all store-scoped operations go through this.
///
/// The runner is the instance's parallelism: every fan-out (ingest, cache
/// update, diff reads, checkout writes) runs over it. The default is the
/// inline [`Sequential`] runner; a host embedding composes a parallel one
/// (the `sekai` binary's OS-thread pool) through [`Self::new`] /
/// [`Self::open_with`] / [`Self::init_with`].
pub struct SekaiInstance<S: SekaiStore, T: ThreadRunner = Sequential> {
    pub(crate) store: S,
    pub(crate) runner: T,
}

impl<S: SekaiStore, T: ThreadRunner> SekaiInstance<S, T> {
    /// Compose an instance from an already opened store and its runner.
    pub const fn new(store: S, runner: T) -> Self {
        Self { store, runner }
    }

    /// Store handle (metadata plus CAS) for escape-hatch access: CAS
    /// operations take no lock, and metadata writes go through their own
    /// transactions.
    pub const fn store(&self) -> &S {
        &self.store
    }

    /// Mutable store handle for escape-hatch access; see [`Self::store`].
    pub const fn store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    /// The runner every fan-out of this instance executes over.
    pub const fn runner(&self) -> &T {
        &self.runner
    }

    /// Read-only handle rooted at `root`.
    pub const fn world<'a, W: WorldTree<Error = WorldError>>(
        &'a self,
        world: &'a W,
    ) -> WorldHandle<'a, W, S, T> {
        WorldHandle {
            instance: self,
            world,
        }
    }

    /// Read-write handle rooted at `root`.
    pub const fn world_mut<'a, W: WorldTree<Error = WorldError>>(
        &'a mut self,
        world: &'a mut W,
    ) -> WorldHandleMut<'a, W, S, T> {
        WorldHandleMut {
            instance: self,
            world,
        }
    }

    /// List all commits in local record order (for `log` and pre-flight checks).
    pub async fn list_commits(&self) -> Result<Vec<Commit>, AppError> {
        sekai_storage::graph::list_commits(self.store.meta())
            .await
            .map_err(AppError::Commit)
    }

    /// The commits a commit continues: its direct parents.
    ///
    /// Fails when the commit does not exist.
    pub async fn commit_parents(&self, commit: CommitId) -> Result<Vec<CommitId>, AppError> {
        sekai_storage::graph::commit_parents(self.store.meta(), commit)
            .await
            .map_err(AppError::Commit)
    }

    /// Every ancestor of a commit, nearest first.
    ///
    /// Fails when the commit does not exist.
    pub async fn commit_ancestors(&self, commit: CommitId) -> Result<Vec<CommitId>, AppError> {
        sekai_storage::graph::commit_ancestors(self.store.meta(), commit)
            .await
            .map_err(AppError::Commit)
    }

    /// The history of a commit: the commit itself followed by its
    /// ancestors, nearest first.
    ///
    /// Fails when the commit does not exist.
    pub async fn commit_history(&self, commit: CommitId) -> Result<Vec<Commit>, AppError> {
        sekai_storage::graph::commit_history(self.store.meta(), commit)
            .await
            .map_err(AppError::Commit)
    }

    /// Latest commit ID; fails when the store has no commits.
    ///
    /// This is the newest commit of the store, not the one the next
    /// commit would continue - see [`SekaiInstance::current_commit_id`].
    pub async fn latest_commit_id(&self) -> Result<CommitId, AppError> {
        sekai_storage::graph::latest_commit_id(self.store.meta())
            .await
            .map_err(AppError::Commit)?
            .ok_or(AppError::NoCommits)
    }

    /// Resolve a revision (`HEAD`, a commit ID, or a branch, with an
    /// optional `~<n>`) to a commit.
    pub async fn resolve_revision(&self, revision: &Revision) -> Result<CommitId, AppError> {
        sekai_storage::graph::resolve_revision(self.store.meta(), revision)
            .await
            .map_err(AppError::Revision)
    }

    /// Count fresh and effective rows for one commit.
    pub async fn commit_stats(
        &self,
        commit: CommitId,
    ) -> Result<sekai_storage::CommitStats, AppError> {
        sekai_storage::graph::commit_stats(self.store.meta(), commit)
            .await
            .map_err(AppError::Commit)
    }

    /// Wipe the region-image cache, returning the number of entries removed.
    ///
    /// The cache is derived state (ADR-0029): cleaning it only costs
    /// performance on later commits, never changes their outcome, so it is
    /// always safe to run for capacity reduction. The next commit rebuilds
    /// the entries it needs.
    pub async fn clean_cache(&mut self) -> Result<u64, AppError> {
        self.store
            .cache_mut()
            .clear()
            .await
            .map_err(AppError::Storage)
    }
}

impl<S: SekaiStore + SekaiStoreOpener> SekaiInstance<S, Sequential> {
    /// Open the existing store of an explicitly chosen backend type, with
    /// the inline default runner; compose a parallel one through
    /// [`Self::open_with`].
    pub async fn open(path: impl AsRef<S::Path>) -> Result<Self, AppError> {
        Self::open_with(path, Sequential::new(0)).await
    }

    /// Initialize the store of an explicitly chosen backend type and open
    /// it, with the inline default runner; the only entry point that
    /// materializes a store. Initializing an existing store again is
    /// idempotent.
    pub async fn init(path: impl AsRef<S::Path>) -> Result<Self, AppError> {
        Self::init_with(path, Sequential::new(0)).await
    }
}

impl<S: SekaiStore + SekaiStoreOpener, T: ThreadRunner> SekaiInstance<S, T> {
    /// Open the existing store of an explicitly chosen backend type,
    /// composing it with `runner`; see [`SekaiInstance::open`].
    pub async fn open_with(path: impl AsRef<S::Path>, runner: T) -> Result<Self, AppError> {
        Ok(Self::new(S::open(path).await?, runner))
    }

    /// Initialize the store of an explicitly chosen backend type and open
    /// it, composing it with `runner`; see [`SekaiInstance::init`].
    pub async fn init_with(path: impl AsRef<S::Path>, runner: T) -> Result<Self, AppError> {
        Ok(Self::new(S::init(path).await?, runner))
    }
}

/// A world lock shared (`Arc`) into the runner jobs that touch files.
///
/// The lock is released when the last share drops. A cancelled future drops
/// its own share at once, but a job already running cannot be aborted, so
/// each one holds a share of its own: the world stays locked until every
/// file it is reading or writing is done.
#[derive(Debug)]
pub(crate) struct WorldGuard<L> {
    /// Held for its drop: the lock must outlive the operation.
    _world: WorldLockState<L>,
}

impl<L: WorldLock> WorldGuard<L> {
    pub(crate) fn acquire<W: WorldTree<Error = WorldError, Lock = L>>(
        world: &W,
        mode: WorldLockMode,
    ) -> Result<Arc<Self>, AppError> {
        Ok(Arc::new(Self {
            _world: world.lock(mode)?,
        }))
    }
}

/// Read-only handle to one world directory bound to an instance.
pub struct WorldHandle<'a, W, S: SekaiStore, T: ThreadRunner = Sequential> {
    pub(crate) instance: &'a SekaiInstance<S, T>,
    pub(crate) world: &'a W,
}

/// Read-write handle to one world directory bound to an instance.
///
/// Runs that write (`commit`, `checkout`) go through `&mut self`, and the
/// handle borrows the instance mutably: while a write session is alive, no
/// second handle for the same instance can exist, so store-scoped
/// operations and other world writes cannot start.
pub struct WorldHandleMut<'a, W, S: SekaiStore, T: ThreadRunner = Sequential> {
    pub(crate) instance: &'a mut SekaiInstance<S, T>,
    pub(crate) world: &'a mut W,
}

impl<W, S: SekaiStore, T: ThreadRunner> WorldHandleMut<'_, W, S, T> {
    /// Read-only view of the same world, for read operations issued while a
    /// write session is held.
    pub(crate) const fn view(&self) -> WorldHandle<'_, W, S, T> {
        WorldHandle {
            instance: &*self.instance,
            world: &*self.world,
        }
    }
}
