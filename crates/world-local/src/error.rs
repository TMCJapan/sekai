//! World access failures with the affected file attached.

use std::path::PathBuf;

use sekai_core::{Dimension, RegionKey, RegionKind};

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
    /// Region file failed to parse, with its location attached for diagnosis.
    #[error("failed to process region file {path}: {source}", path = path.display())]
    RegionFile {
        /// The file that failed to parse.
        path: PathBuf,
        /// Underlying parse failure.
        #[source]
        source: sekai_anvil::AnvilError,
    },
    /// Another operation holds a conflicting lock on the world.
    #[error(
        "world is in use by another sekai operation (lock {path}): wait for it to finish, or stop the operation holding it, then retry",
        path = path.display()
    )]
    WorldLocked {
        /// Lock file that could not be taken.
        path: PathBuf,
    },
    /// No directory mapping exists for this namespace.
    #[error("cannot derive region path for dim {dim_code}, kind {kind_code}, r.{region_x}.{region_z}", dim_code = dim.raw(), kind_code = kind.raw())]
    UnknownRegionPath {
        /// Dimension namespace.
        dim: Dimension,
        /// Region family.
        kind: RegionKind,
        /// Region X.
        region_x: i32,
        /// Region Z.
        region_z: i32,
    },
    /// A `server.properties` beside a world has only invalid `level-name`
    /// values.
    ///
    /// Never raised by this crate: reading it for detection treats such a
    /// file like an unreadable one and falls back to directory election.
    /// Raised by callers that treat a lost `level-name` as fatal, such as
    /// `worktree add`.
    #[error("{path} has no valid level-name", path = path.display())]
    InvalidLevelName {
        /// The `server.properties` file.
        path: PathBuf,
    },
    /// An internal invariant broke: a dimension directory lay outside the
    /// world root. Constructors root every directory under `root`, so this
    /// indicates a programming error, not an environment failure.
    #[error("dimension directory {path} lies outside the world root", path = path.display())]
    Invariant {
        /// The offending dimension directory.
        path: PathBuf,
    },
    /// Two region files spell the same region identity.
    ///
    /// Raised by discovery, which refuses to pick one of the two files: a
    /// restore or a write never raises it.
    #[error("two region files spell the same region ({region_x},{region_z}) of dim {dim_code}, kind {kind_code}: {first} and {second}", dim_code = dim.raw(), kind_code = kind.raw(), first = first.display(), second = second.display())]
    AmbiguousRegion {
        /// Dimension namespace.
        dim: Dimension,
        /// Region family.
        kind: RegionKind,
        /// Region X both files spell.
        region_x: i32,
        /// Region Z both files spell.
        region_z: i32,
        /// First file.
        first: PathBuf,
        /// Second file.
        second: PathBuf,
    },
}

impl WorldError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    /// Attach the key's identity to a path the tree cannot derive for it,
    /// so every call site spells the error the same way.
    pub(crate) const fn unknown_region_path(key: RegionKey) -> Self {
        Self::UnknownRegionPath {
            dim: key.dim,
            kind: key.kind,
            region_x: key.rx,
            region_z: key.rz,
        }
    }

    /// Attach a region file's location to a parse failure, so every call
    /// site spells the error the same way.
    pub(crate) fn region_file(path: impl Into<PathBuf>, source: sekai_anvil::AnvilError) -> Self {
        Self::RegionFile {
            path: path.into(),
            source,
        }
    }
}
