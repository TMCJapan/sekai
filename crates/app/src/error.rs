//! Composition failures with their source attached.

use std::fmt::Display;

use sekai_core::RegionKey;

/// Failure for a world tree that reports one region identity twice.
pub(crate) fn duplicate_region(
    key: &RegionKey,
    first: &impl Display,
    second: &impl Display,
) -> AppError {
    AppError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("world tree enumerated region {key:?} twice ({first} and {second})"),
    ))
}

/// Failures while committing, checking out, or listing commits.
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
    /// Region file failed to parse, with its location attached for diagnosis.
    #[error("failed to process region file {location}: {source}")]
    RegionFailed {
        /// Where the region file lives.
        location: String,
        /// Underlying parse failure.
        #[source]
        source: sekai_anvil::AnvilError,
    },
    /// Checkout plan could not be resolved.
    #[error("{0}")]
    Checkout(sekai_core::CheckoutError<sekai_storage::StorageError>),
    /// Garbage collection failed.
    #[error("{0}")]
    Gc(sekai_core::GcError<sekai_storage::StorageError, sekai_storage::StorageError>),
    /// Commit pruning failed.
    #[error("{0}")]
    Prune(sekai_core::PruneError<sekai_storage::StorageError>),
    /// NBT parsing or diff operation failed.
    #[error(transparent)]
    Nbt(#[from] sekai_nbt::NbtError),
    /// Commit-graph operation failed.
    #[error("{0}")]
    Commit(sekai_core::CommitError<sekai_storage::StorageError>),
    /// Commit reference (`<id>` or `@tag`) could not be resolved.
    #[error("{0}")]
    CommitRef(sekai_core::ResolveError<sekai_storage::StorageError>),
    /// Tag name is already taken.
    #[error("tag already exists: {name} (use --force to move it)")]
    TagExists {
        /// Conflicting tag name.
        name: String,
    },
    /// Ref name is already taken.
    #[error("ref already exists: {name} (use --force to move it)")]
    RefExists {
        /// Conflicting ref name.
        name: String,
    },
    /// The named ref does not exist.
    #[error("unknown ref: {name}")]
    UnknownRef {
        /// Ref that was asked for.
        name: String,
    },
    /// The active ref cannot be removed.
    #[error("cannot remove the active ref: {name} (activate another ref first)")]
    ActiveRef {
        /// Ref that was asked for.
        name: String,
    },
    /// No commits exist in the store.
    #[error("no commits found in store")]
    NoCommits,
    /// I/O operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// System clock went backwards or overflowed.
    #[error("system clock error: {0}")]
    Clock(#[from] std::time::SystemTimeError),
    /// System clock reading does not fit a Unix-millisecond `u64`.
    #[error("system clock is out of range")]
    ClockOutOfRange,
    /// Background worker task failed.
    #[error("worker task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}
