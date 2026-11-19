//! Opened store instance and world handles.
//!
//! [`SekaiInstance`] is the single entry point for store-scoped operations:
//! the store is opened once per process instead of once per free function,
//! and metadata/CAS adapters are reached through one place. The store type
//! is pluggable: any [`SekaiStore`] backend can be used, and the local
//! backend additionally gets `open_location` and `init_world`, which tie
//! the store to the world a command runs in (ADR-0035).
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

use std::path::PathBuf;
use std::sync::Arc;

use sekai_core::{CommitId, Revision, Target, WorktreeName};
use sekai_storage::{CacheStore as _, SekaiStore};
use sekai_world::{WorldLock, WorldLockMode, WorldLockState, WorldTree};

use crate::error::AppError;
use crate::refs::switch_err;

/// An opened store; all store-scoped operations go through this.
pub struct SekaiInstance<T: SekaiStore> {
    pub(crate) store: T,
    /// The worktree commands act on, or `None` outside every worktree,
    /// where reading HEAD fails.
    pub(crate) worktree: Option<WorktreeName>,
    /// World root of the main worktree, the directory that holds the
    /// store, when the store lies on the local filesystem. Errors name a
    /// worktree by its root, as Git's do.
    pub(crate) main_root: Option<PathBuf>,
}

impl<T: SekaiStore> SekaiInstance<T> {
    /// Compose an instance from an already opened store, outside every
    /// worktree.
    pub const fn new(store: T) -> Self {
        Self {
            store,
            worktree: None,
            main_root: None,
        }
    }

    /// The same instance acting on `worktree`, or outside every worktree
    /// for `None`.
    #[must_use]
    pub fn with_worktree(self, worktree: Option<WorktreeName>) -> Self {
        Self { worktree, ..self }
    }

    /// The worktree commands act on, or `None` outside every worktree.
    pub const fn worktree(&self) -> Option<&WorktreeName> {
        self.worktree.as_ref()
    }

    /// The worktree commands act on; fails outside every worktree, as a
    /// command that writes HEAD must.
    pub(crate) fn current_worktree(&self) -> Result<&WorktreeName, AppError> {
        self.worktree
            .as_ref()
            .ok_or(AppError::CommitRef(sekai_core::ResolveError::NoWorktree))
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

    /// The commits a commit continues: its direct parents.
    ///
    /// Fails when the commit does not exist.
    pub async fn commit_parents(&self, commit: CommitId) -> Result<Vec<CommitId>, AppError> {
        sekai_core::usecase::graph::commit_parents(self.store.meta(), commit)
            .await
            .map_err(AppError::Commit)
    }

    /// Resolve `revision` against the store and the worktree's HEAD.
    pub async fn resolve_revision(&self, revision: &Revision) -> Result<CommitId, AppError> {
        sekai_core::resolve_revision(self.store.meta(), self.worktree.as_ref(), revision)
            .await
            .map_err(switch_err)
    }

    /// Resolve a `checkout` target: an argument that may name a branch,
    /// as [`WorldHandleMut::switch`] takes it.
    pub async fn resolve_target(&self, revision: &Revision) -> Result<Target, AppError> {
        sekai_core::resolve_target(self.store.meta(), self.worktree.as_ref(), revision)
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

/// A world lock shared (`Arc`) into the blocking tasks that touch files.
///
/// The lock is released when the last share drops. A cancelled future drops
/// its own share at once, but a blocking task cannot be aborted once it has
/// started, so each one holds a share of its own: the world stays locked
/// until every file it is reading or writing is done.
pub(crate) struct WorldGuard<L> {
    /// Held for its drop: the lock must outlive the operation.
    _world: WorldLockState<L>,
    /// Dropped after `_world` (fields drop in declaration order), so it
    /// runs once the last share is gone and the lock is released.
    _then: Option<Box<dyn Send + Sync>>,
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
            _world: world.lock(mode)?,
            _then: None,
        }))
    }

    /// [`Self::acquire`], dropping `then` once every share is gone: after
    /// a failure or a cancellation, that is when the last blocking task
    /// touching the world ends.
    #[cfg(feature = "backend-local")]
    pub(crate) fn acquire_then<W: WorldTree<Lock = L>>(
        world: &W,
        mode: WorldLockMode,
        then: impl Send + Sync + 'static,
    ) -> Result<Arc<Self>, AppError>
    where
        AppError: From<W::Error>,
    {
        Ok(Arc::new(Self {
            _world: world.lock(mode)?,
            _then: Some(Box::new(then)),
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
