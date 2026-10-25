//! Composition/orchestration library for third parties.
//!
//! Assembles `core` policy over `world` observations and `storage` backends
//! selected by URL. Threading, clocks, and timing live here because `core`
//! is `no_std`; argument parsing and output formatting live in the `sekai`
//! binary.
//!
//! Store-scoped operations hang off [`SekaiInstance`] (open once, then
//! `gc`, `prune`, `list_commits`, ...); operations that touch a world
//! hang off the handles it hands out: [`WorldHandle`] for read-only work
//! (`status`, `diff_world_*`) and [`WorldHandleMut`] for `commit` and
//! `checkout`.
//!
//! A world is any [`WorldTree`]: the operations reach its region files
//! through [`RegionFile`] handles, so a tree that is not backed by a
//! filesystem runs the same flows as one that is.

mod checkout;
mod clone;
mod commit;
mod diff;
mod error;
mod gc;
mod instance;
mod prune;
mod refs;
mod regions;
mod scopes;
mod time;

pub use checkout::{
    CheckoutOptions, CheckoutProgress, CheckoutTimings, MissingBlobPolicy, MissingFilePolicy,
};
pub use clone::{CloneOptions, CloneProgress, CloneReport, CloneTimings};
pub use commit::{
    CommitOptions, CommitProgress, CommitTimings, RegionTiming, StatusOptions, StatusReport,
    StatusTimings,
};
pub use diff::{ChunkDiff, DiffProgress, DiffTimings, world_chunk_coords};
pub use error::AppError;
pub use gc::{GcProgress, GcTimings};
pub use instance::{SekaiInstance, WorldHandle, WorldHandleMut};
pub use prune::{PruneProgress, PruneTimings};
pub use sekai_core::{
    Area, BlobHash, CheckoutReport, ChunkCoord, Commit, CommitId, CommitRef, CommitReport,
    CommitStats, DEFAULT_REF_NAME, Dimension, GcPlan, GcReport, NamedScope, NamedScopeError,
    PrunePlan, PruneReport, Rect, RefName, RefNameError, RegionFingerprint, RegionKey, RegionKind,
    Scope, ScopeName, ScopeNameError,
};
pub use sekai_storage::{BlockingBlobStore, SekaiStore, SekaiStoreOpener};
pub use sekai_world::{
    HostRegion, HostWorldLock, HostWorldLockError, HostWorldTree, Region, RegionFile,
    UnguardedReason, WorldError, WorldLock, WorldLockMode, WorldLockState, WorldTree,
};
