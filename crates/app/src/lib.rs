//! Composition/orchestration library for third parties.
//!
//! Assembles `core` policy over `world` observations and `storage` backends
//! selected by URL. Parallelism runs over a
//! [`ThreadRunner`](sekai_platform::thread::ThreadRunner) composed into the
//! instance (the default is the inline
//! [`Sequential`](sekai_platform::thread::Sequential) runner; the `sekai`
//! binary composes its OS-thread pool), and clocks and timing run over a
//! [`Clock`](sekai_platform::time::Clock) composed the same way (the default
//! [`Zero`](sekai_platform::time::Zero) reads no time; the `sekai` binary
//! composes its OS clock). Argument parsing and output formatting live in
//! the `sekai` binary. No async runtime is named here: the flows are
//! executor-agnostic, and a `no_std`/wasm embedding supplies its own runner
//! and clock.
//!
//! Store-scoped operations hang off [`SekaiInstance`] (open once, then
//! `list_commits`, ...); operations that touch a world hang off the handles
//! it hands out: [`WorldHandle`](crate::instance::WorldHandle) for read-only
//! work (`status`, `diff_world_*`) and
//! [`WorldHandleMut`](crate::instance::WorldHandleMut) for runs that write
//! the store or the world (`commit`, `checkout`, `update_cache`).
//! [`SekaiInstance::restore`] composes a source resolution with a scoped
//! checkout under git-restore defaults.
//!
//! A world is any [`WorldTree`](sekai_world::WorldTree): the operations reach
//! its region files through [`RegionFile`](sekai_world::RegionFile) handles,
//! so a tree that is not backed by a filesystem runs the same flows as one
//! that is.

#![no_std]

extern crate alloc;

mod cache;
mod checkout;
mod commit;
mod diff;
mod error;
pub mod instance;
mod progress;
mod refs;
mod regions;
mod restore;
mod scopes;
mod time;
mod workers;

pub use cache::{CacheUpdateOptions, CacheUpdateReport, CacheUpdateTimings};
pub use checkout::{
    CheckoutError, CheckoutOptions, CheckoutReport, CheckoutTimings, MissingBlobPolicy,
    MissingFilePolicy,
};
pub use commit::{
    CommitOptions, CommitReport, CommitTimings, RegionTiming, StatusOptions, StatusReport,
    StatusTimings,
};
pub use diff::{ChunkDiff, DiffTimings, world_chunk_coords};
pub use error::{AppError, Lookup, NameKind};
pub use instance::SekaiInstance;
pub use progress::{BuildProgress, Progress};
pub use refs::ListedBranch;
pub use restore::RestoreOptions;
pub use sekai_storage::{CommitStats, SekaiStore, SekaiStoreOpener};
pub use sekai_util::{
    Area, ChunkHash, Commit, CommitId, CommitRef, DEFAULT_REF_NAME, DimHash, Dimension, NamedScope,
    Rect, Ref, RefKind, RefName, RegionFingerprint, RegionHash, RegionKey, RegionKind, Revision,
    RevisionBase, RootHash, Scope, ScopeName,
    chunk_coord::ChunkCoord,
    error::*,
    worktree::{Head, HeadState},
};
