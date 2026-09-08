//! Concrete error type for the composed entry points.
//!
//! Rationale: the composition root maps every layer's failure into one enum
//! so the binary prints a single coherent error. Format and storage errors
//! pass through untouched (`#[from]`); only orchestration-level situations
//! (unknown snapshot, broken clock) get dedicated variants. Core use-case
//! errors over the storage adapters fold into the matching variant.

use std::io;
use std::path::PathBuf;

/// Failures while discovering, backing up, or rolling back worlds.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// File-system operation failed.
    #[error("file I/O failed for {path}: {source}", path = path.display())]
    Io {
        /// File or directory involved.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: io::Error,
    },

    /// Region file failed to parse or rebuild.
    #[error("region file failed: {0}")]
    Mca(#[from] sekai_mca::McaError),

    /// Blob or metadata persistence failed.
    #[error("storage failed: {0}")]
    Storage(#[from] sekai_storage::StorageError),

    /// No snapshot with this ID exists.
    #[error("unknown snapshot: {id}")]
    UnknownSnapshot {
        /// Requested snapshot ID.
        id: u64,
    },

    /// System clock is unusable (time before the Unix epoch).
    #[error("system clock failed: {0}")]
    Clock(#[from] std::time::SystemTimeError),
}

impl From<sekai_core::usecase::rollback::RollbackError<sekai_storage::StorageError>> for Error {
    fn from(
        err: sekai_core::usecase::rollback::RollbackError<sekai_storage::StorageError>,
    ) -> Self {
        match err {
            sekai_core::usecase::rollback::RollbackError::Meta(source) => Self::Storage(source),
            sekai_core::usecase::rollback::RollbackError::UnknownSnapshot { id } => {
                Self::UnknownSnapshot { id }
            }
        }
    }
}
