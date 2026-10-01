//! Composition/orchestration library for third parties.
//!
//! Assembles `core` policy over `world` observations and `storage` backends
//! selected by URL. Threading, clocks, and timing live here because `core`
//! is `no_std`; argument parsing and output formatting live in the `sekai`
//! binary.
//!
//! Store-scoped operations hang off [`SekaiInstance`] (open once, then
//! `gc`, `prune`, `list_snapshots`, ...); operations that touch a world
//! hang off the handles it hands out: [`WorldHandle`] for read-only work
//! (`status`, `diff_world_*`) and [`WorldHandleMut`] for `backup` and
//! `rollback`.

mod backup;
mod diff;
mod error;
mod export;
mod gc;
mod instance;
mod prune;
mod rollback;
mod tag;

pub use backup::{
    BackupOptions, BackupProgress, BackupTimings, RegionTiming, StatusOptions, StatusReport,
    StatusTimings,
};
pub use diff::{ChunkDiff, DiffProgress, DiffTimings, world_chunk_coords};
pub use error::AppError;
pub use export::{ExportOptions, ExportProgress, ExportReport, ExportTimings};
pub use gc::{GcProgress, GcTimings};
pub use instance::{SekaiInstance, WorldHandle, WorldHandleMut};
pub use prune::{PruneProgress, PruneTimings};
pub use rollback::{
    MissingBlobPolicy, MissingFilePolicy, RollbackOptions, RollbackProgress, RollbackTimings,
};
pub use sekai_core::{
    Area, BackupReport, BlobHash, ChunkCoord, DEFAULT_IGNORED, Dimension, GcPlan, GcReport,
    NbtChange, NbtDiffEntry, NbtValue, PrunePlan, PruneReport, Rect, RegionKey, RegionKind,
    RollbackReport, Scope, Snapshot, SnapshotId, SnapshotStats, SnapshotTag, TagName,
};
pub use sekai_world::{LayoutFlavor, RegionScanEntry, ScanReport, ScanSkip, ScanTimings};

/// Read-only inspection of every region file under `world`, additionally
/// returning per-phase timings and any files that could not be inspected.
///
/// Never writes to the world or the store.
pub fn scan(world: &std::path::Path) -> Result<ScanReport, AppError> {
    Ok(sekai_world::scan_world(world)?)
}
