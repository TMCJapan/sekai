//! Composition failures with their source attached.

use std::path::PathBuf;

/// Failures while backing up, rolling back, or listing snapshots.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// World filesystem operation failed.
    #[error(transparent)]
    World(#[from] sekai_world::WorldError),
    /// Storage backend operation failed.
    #[error(transparent)]
    Storage(#[from] sekai_storage::StorageError),
    /// Region image failed to parse or build.
    #[error(transparent)]
    Anvil(#[from] sekai_anvil::AnvilError),
    /// Region file failed to parse, with the file attached for diagnosis.
    #[error("failed to process region file {}: {source}", path.display())]
    RegionFailed {
        /// File involved.
        path: PathBuf,
        /// Underlying parse failure.
        #[source]
        source: sekai_anvil::AnvilError,
    },
    /// Rollback plan could not be resolved.
    #[error("{0}")]
    Rollback(sekai_core::RollbackError<sekai_storage::StorageError>),
    /// Garbage collection failed.
    #[error("{0}")]
    Gc(sekai_core::GcError<sekai_storage::StorageError, sekai_storage::StorageError>),
    /// Snapshot pruning failed.
    #[error("{0}")]
    Prune(sekai_core::PruneError<sekai_storage::StorageError>),
    /// NBT parsing or diff operation failed.
    #[error(transparent)]
    Nbt(#[from] sekai_nbt::NbtError),
    /// Snapshot operation failed.
    #[error("{0}")]
    Snapshot(sekai_core::usecase::snapshot::SnapshotError<sekai_storage::StorageError>),
    /// Snapshot reference (`<id>` or `@tag`) could not be resolved.
    #[error("{0}")]
    SnapshotRef(sekai_core::usecase::snapshot::ResolveError<sekai_storage::StorageError>),
    /// Tag name is already taken.
    #[error("tag already exists: {name} (use --force to move it)")]
    TagExists {
        /// Conflicting tag name.
        name: String,
    },
    /// No snapshots exist in the store.
    #[error("no snapshots found in store")]
    NoSnapshots,
    /// I/O operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// System clock went backwards or overflowed.
    #[error("system clock error: {0}")]
    Clock(#[from] std::time::SystemTimeError),
    /// Background worker task failed.
    #[error("worker task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}
