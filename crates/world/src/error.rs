//! Filesystem failures with the affected path attached.

use std::path::PathBuf;

/// Failures while discovering, reading, or swapping world files.
#[derive(Debug, thiserror::Error)]
pub enum WorldError {
    /// File-system operation failed.
    #[error("file I/O failed for {path}: {source}", path = path.display())]
    Io {
        /// File (or directory, for fsync) involved.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// Region image failed to parse or build.
    #[error("region image error: {0}")]
    Anvil(#[from] sekai_anvil::AnvilError),
    /// No directory mapping exists for this namespace.
    #[error("cannot derive region path for dim {dim}, kind {kind}, r.{region_x}.{region_z}")]
    UnknownRegionPath {
        /// Dimension namespace code.
        dim: i32,
        /// Region family code.
        kind: i32,
        /// Region X.
        region_x: i32,
        /// Region Z.
        region_z: i32,
    },
}

impl WorldError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
