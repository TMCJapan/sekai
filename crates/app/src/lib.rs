//! Composition/orchestration library for third parties.
//!
//! Assembles `core` policy over `world` observations and `storage` backends
//! selected by URL. Parallelism runs over a [`ThreadRunner`] composed into
//! the instance (the default is the inline [`Sequential`] runner; the
//! `sekai` binary composes its OS-thread pool); clocks and timing live
//! here because `core` is `no_std`; argument parsing and output formatting
//! live in the `sekai` binary. No async runtime is named here: the flows
//! are executor-agnostic, and a `no_std`/wasm embedding supplies its own
//! runner.
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
mod progress;
mod refs;
mod regions;
mod scopes;
mod time;
mod workers;

pub use cache::{CacheUpdateOptions, CacheUpdateReport, CacheUpdateTimings};
pub use checkout::{
    CheckoutError, CheckoutOptions, CheckoutPlan, CheckoutReport, CheckoutTimings,
    MissingBlobPolicy, MissingFilePolicy,
};
pub use commit::{
    CommitOptions, CommitReport, CommitTimings, RegionTiming, StatusOptions, StatusReport,
    StatusTimings,
};
pub use diff::{ChunkDiff, DiffTimings, world_chunk_coords};
pub use error::{AppError, Lookup, NameKind};
pub use instance::{SekaiInstance, WorldHandle, WorldHandleMut};
pub use progress::{BuildProgress, Progress};
pub use sekai_core::{
    Area, ChunkCoord, ChunkHash, Commit, CommitId, CommitRef, DEFAULT_REF_NAME, DimHash, Dimension,
    NamedScope, NamedScopeError, Rect, Ref, RefKind, RefName, RefNameError, RegionFingerprint,
    RegionHash, RegionKey, RegionKind, Revision, RevisionBase, RevisionError, RootHash, Scope,
    ScopeName, ScopeNameError,
};
pub use sekai_storage::{CommitStats, SekaiStore, SekaiStoreOpener};
pub use sekai_util::{JobSet, Sequential, ThreadRunner, WorkerPanic};
pub use sekai_world::{
    Region, RegionFile, UnguardedReason, WorldLock, WorldLockMode, WorldLockState, WorldTree,
};
#[cfg(feature = "backend-local")]
pub use sekai_world_local::{
    HostRegion, HostWorldLock, HostWorldLockError, HostWorldTree, WorldError,
};
