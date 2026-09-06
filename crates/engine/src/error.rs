//! Error type for backup and rollback orchestration.
//!
//! Rationale: `engine` maps every layer's failure into one enum so the CLI
//! prints a single coherent error. Format and storage errors pass through
//! untouched (`#[from]`); only orchestration-level situations (unknown
//! snapshot, unmappable region path, broken clock) get dedicated variants.

use std::io;
use std::path::PathBuf;

use sekai_core::{Dimension, RegionKind};

/// Failures while discovering, backing up, or rolling back worlds.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
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

    /// No directory mapping exists for this coordinate's namespace.
    ///
    /// Vanilla namespaces are always mappable; this fires for hashed
    /// custom dimensions whose on-disk file is gone (the hash is one-way).
    #[error("cannot derive region path for dim {dim}, kind {kind}, r.{region_x}.{region_z}",
        dim = .dim.raw(), kind = .kind.raw())]
    UnknownRegionPath {
        /// Dimension namespace code.
        dim: Dimension,
        /// Region family code.
        kind: RegionKind,
        /// Region X.
        region_x: i32,
        /// Region Z.
        region_z: i32,
    },

    /// System clock is unusable (time before the Unix epoch).
    #[error("system clock failed: {0}")]
    Clock(#[from] std::time::SystemTimeError),
}
