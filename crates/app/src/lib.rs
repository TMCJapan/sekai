//! Composition/orchestration library for third parties.
//!
//! Assembles `core` policy over `world` observations and `storage` backends
//! selected by URL. Threading, clocks, and timing live here because `core`
//! is `no_std`; argument parsing and output formatting live in the `sekai`
//! binary.

mod backup;
mod diff;
mod error;
mod gc;
mod rollback;

pub use backup::{BackupOptions, BackupProgress, BackupTimings, RegionTiming, backup};
pub use diff::{
    ChunkDiff, DiffProgress, DiffTimings, diff_blobs, diff_chunk, diff_chunks, diff_world_chunk,
    diff_world_chunks, snapshot_chunk_coords, world_chunk_coords,
};
pub use error::AppError;
pub use gc::{GcProgress, GcTimings, gc, gc_apply, gc_plan};
pub use rollback::{
    MissingBlobPolicy, MissingFilePolicy, RollbackOptions, RollbackProgress, RollbackTimings,
    rollback,
};
pub use sekai_core::{
    Area, BackupReport, BlobHash, ChunkCoord, DEFAULT_IGNORED, Dimension, GcPlan, GcReport,
    NbtChange, NbtDiffEntry, NbtValue, Rect, RegionKey, RegionKind, RollbackReport, Scope,
    Snapshot, SnapshotId,
};
pub use sekai_world::{RegionScanEntry, ScanTimings};

/// List all snapshots in ID order (for `list` and pre-flight checks).
pub async fn list_snapshots(store_url: &str) -> Result<Vec<Snapshot>, AppError> {
    let store = open_store(store_url).await?;
    sekai_core::usecase::snapshot::list_snapshots(store.meta())
        .await
        .map_err(AppError::Snapshot)
}

pub async fn latest_snapshot_id(store_url: &str) -> Result<SnapshotId, AppError> {
    let store = open_store(store_url).await?;
    sekai_core::usecase::snapshot::latest_snapshot_id(store.meta())
        .await
        .map_err(AppError::Snapshot)?
        .ok_or(AppError::NoSnapshots)
}

/// Read-only inspection of every region file under `world`, additionally
/// returning per-phase timings.
///
/// Never writes to the world or the store.
pub fn scan(world: &std::path::Path) -> Result<(Vec<RegionScanEntry>, ScanTimings), AppError> {
    Ok(sekai_world::scan_world(world)?)
}

/// Open the store named by `store_url`.
///
/// `sqlite://<dir>` (or a bare `<dir>`) opens a SQLite store; anything else
/// fails loudly, including backends not compiled in.
async fn open_store(store_url: &str) -> Result<sekai_storage::SqliteStore, AppError> {
    let (kind, rest) = sekai_storage::parse_backend_url(store_url)?;
    // `Sqlite` is currently the only variant; the pattern becomes
    // refutable once stub backends land, and anything else fails below.
    #[cfg(feature = "backend-sqlite")]
    if kind == sekai_storage::BackendKind::Sqlite {
        return Ok(sekai_storage::open_sqlite(std::path::Path::new(rest)).await?);
    }
    Err(sekai_storage::StorageError::UnsupportedBackend {
        url: store_url.to_owned(),
    }
    .into())
}
