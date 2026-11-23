//! Backend-neutral world access failures.

use alloc::boxed::Box;
use alloc::string::String;
use core::fmt;

use sekai_anvil::AnvilError;
use sekai_core::RegionKey;

/// Failures while discovering, reading, locking, or writing world files.
///
/// The composition layer runs trees whose
/// [`WorldTree::Error`](crate::WorldTree::Error) is this type, the way it
/// runs store backends whose `Error` is `StorageError`, so it converts the
/// failure without knowing which tree runs. A tree-specific cause travels
/// boxed inside [`WorldError::Io`];
/// locations are the tree's own spelling of where a file lives, since the
/// boundary carries no paths.
#[derive(Debug)]
pub enum WorldError {
    /// Storage operation of the tree failed.
    Io {
        /// File (or directory, for fsync) involved.
        location: String,
        /// Underlying tree-specific failure.
        source: Box<dyn core::error::Error + Send + Sync>,
    },
    /// Region image failed to parse or build.
    Anvil(AnvilError),
    /// Region file failed to parse, with its location attached for diagnosis.
    RegionFile {
        /// The file that failed to parse.
        location: String,
        /// Underlying parse failure.
        source: AnvilError,
    },
    /// Another operation holds a conflicting lock on the world.
    Locked {
        /// Lock that could not be taken.
        location: String,
    },
    /// The tree's layout names no file for this region.
    UnknownRegionPath {
        /// Region whose file could not be derived.
        key: RegionKey,
    },
    /// Two region files spell the same region identity.
    ///
    /// Raised by discovery, which refuses to pick one of the two files: a
    /// restore or a write never raises it.
    AmbiguousRegion {
        /// Region both files spell.
        key: RegionKey,
        /// First file.
        first: String,
        /// Second file.
        second: String,
    },
}

impl WorldError {
    /// Wrap a tree-specific failure without coupling this crate to the
    /// tree's error type.
    pub fn io(
        location: impl Into<String>,
        source: impl core::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Io {
            location: location.into(),
            source: Box::new(source),
        }
    }

    /// Attach a region file's location to a parse failure, so every call
    /// site spells the error the same way.
    pub fn region_file(location: impl Into<String>, source: AnvilError) -> Self {
        Self::RegionFile {
            location: location.into(),
            source,
        }
    }
}

impl fmt::Display for WorldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { location, source } => write!(f, "file I/O failed for {location}: {source}"),
            Self::Anvil(source) => write!(f, "region image error: {source}"),
            Self::RegionFile { location, source } => {
                write!(f, "failed to process region file {location}: {source}")
            }
            Self::Locked { location } => write!(
                f,
                "world is in use by another sekai operation (lock {location}): wait for it to finish, or stop the operation holding it, then retry"
            ),
            // The troubleshooting chapter of the user guide quotes this wording.
            Self::UnknownRegionPath { key } => write!(
                f,
                "cannot derive region path for dim {}, kind {}, r.{}.{}",
                key.dim.raw(),
                key.kind.raw(),
                key.rx,
                key.rz
            ),
            Self::AmbiguousRegion { key, first, second } => write!(
                f,
                "two region files spell the same region ({},{}) of dim {}, kind {}: {first} and {second}",
                key.rx,
                key.rz,
                key.dim.raw(),
                key.kind.raw()
            ),
        }
    }
}

impl core::error::Error for WorldError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source.as_ref()),
            Self::Anvil(source) | Self::RegionFile { source, .. } => Some(source),
            Self::Locked { .. } | Self::UnknownRegionPath { .. } | Self::AmbiguousRegion { .. } => {
                None
            }
        }
    }
}

impl From<AnvilError> for WorldError {
    fn from(source: AnvilError) -> Self {
        Self::Anvil(source)
    }
}
