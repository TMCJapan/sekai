//! Composition failures with their source attached.

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
    /// Rollback plan could not be resolved.
    #[error("{0}")]
    Rollback(sekai_core::RollbackError<sekai_storage::StorageError>),
    /// Garbage collection failed.
    #[error("{0}")]
    Gc(sekai_core::GcError<sekai_storage::StorageError, sekai_storage::StorageError>),
    /// NBT parsing or diff operation failed.
    #[error(transparent)]
    Nbt(#[from] sekai_nbt::NbtError),
    /// Snapshot operation failed.
    #[error("{0}")]
    Snapshot(sekai_core::usecase::snapshot::SnapshotError<sekai_storage::StorageError>),
    /// Chunk was not found in the specified snapshot.
    #[error("chunk {coord:?} not found in snapshot {snapshot_id:?}")]
    ChunkNotFoundInSnapshot {
        snapshot_id: sekai_core::SnapshotId,
        coord: sekai_core::ChunkCoord,
    },
    /// Chunk was not found in the world filesystem.
    #[error("chunk {coord:?} not found in world")]
    ChunkNotFoundInWorld { coord: sekai_core::ChunkCoord },
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
