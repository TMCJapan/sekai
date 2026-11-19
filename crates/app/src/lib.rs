//! Composition/orchestration library for third parties.
//!
//! Assembles `core` policy over `world` observations and `storage` backends
//! selected by URL. Threading, clocks, and timing live here because `core`
//! is `no_std`; argument parsing and output formatting live in the `sekai`
//! binary.
//!
//! Store-scoped operations hang off [`SekaiInstance`] (open once, then
//! `list_commits`, ...); operations that touch a world
//! hang off the handles it hands out: [`WorldHandle`] for read-only work
//! (`status`, `diff_world_*`) and [`WorldHandleMut`] for runs that write
//! the store or the world (`commit`, `checkout`, `update_cache`).
//!
//! A world is any [`WorldTree`]: the operations reach its region files
//! through [`RegionFile`] handles, so a tree that is not backed by a
//! filesystem runs the same flows as one that is.

mod cache;
mod checkout;
mod commit;
mod diff;
mod error;
mod instance;
#[cfg(feature = "backend-local")]
mod location;
mod log;
mod progress;
mod refs;
mod regions;
mod scopes;
mod time;
mod workers;
#[cfg(feature = "backend-local")]
mod worktree;

pub use cache::{CacheUpdateOptions, CacheUpdateReport, CacheUpdateTimings};
pub use checkout::{
    CheckoutOptions, CheckoutTimings, MissingBlobPolicy, MissingFilePolicy, Restored, Switched,
};
pub use commit::{
    CommitOptions, CommitTimings, RegionTiming, StatusOptions, StatusReport, StatusTimings,
};
pub use diff::{ChunkDiff, DiffTimings, scoped_coords, world_chunk_coords};
pub use error::{AppError, BranchUse, Lookup, NameKind, Unremovable};
pub use instance::{SekaiInstance, WorldHandle, WorldHandleMut};
#[cfg(feature = "backend-local")]
pub use location::{Initialized, Location, SEKAI_DIR, WorldRoot, discover};
pub use log::{Decoration, LogEntry, LogOptions};
pub use progress::{BuildProgress, Progress};
pub use refs::{BranchEntry, RefUpdate, Renamed};
pub use sekai_core::{
    Area, CheckoutReport, ChunkCoord, ChunkHash, Commit, CommitId, CommitRef, CommitReport,
    CommitStats, DEFAULT_REF_NAME, DimHash, Dimension, Head, HeadMove, HeadState, NamedScope,
    NamedScopeError, Rect, Ref, RefKind, RefName, RefNameError, Reflog, ReflogEntry,
    ReflogSelector, RegionFingerprint, RegionHash, RegionKey, RegionKind, Revision, RevisionError,
    RevisionRange, RootHash, Scope, ScopeName, ScopeNameError, Target, WorktreeName, WorktreeStart,
};
pub use sekai_storage::SekaiStore;
#[cfg(feature = "backend-local")]
pub use sekai_storage_local::local_store_exists;
pub use sekai_world::{
    Region, RegionFile, UnguardedReason, WorldLock, WorldLockMode, WorldLockState, WorldTree,
};
#[cfg(feature = "backend-local")]
pub use sekai_world_local::{
    HostRegion, HostWorldLock, HostWorldLockError, HostWorldTree, WorldError,
};
#[cfg(feature = "backend-local")]
pub use worktree::{AddedWorktree, WorktreeEntry};
