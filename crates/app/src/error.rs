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
    /// System clock went backwards or overflowed.
    #[error("system clock error: {0}")]
    Clock(#[from] std::time::SystemTimeError),
    /// Background worker task failed.
    #[error("worker task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}
