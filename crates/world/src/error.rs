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

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::borrow::ToOwned;
    use alloc::format;
    use alloc::string::ToString;
    use core::error::Error as _;
    use sekai_core::{Dimension, RegionKind};

    /// Stand-in tree failure: the boundary only needs `Error`, never a
    /// concrete host I/O type.
    #[derive(Debug)]
    struct TestSource(&'static str);

    impl fmt::Display for TestSource {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.0)
        }
    }

    impl core::error::Error for TestSource {}

    const KEY: RegionKey = RegionKey::new(Dimension::new(4242), RegionKind::Region, -1, 2);

    fn parse_failure() -> AnvilError {
        AnvilError::TruncatedFile { len: 3 }
    }

    #[test]
    fn io_error_keeps_location_and_a_downcastable_source() {
        let err = WorldError::io("w/region/r.0.0.mca", TestSource("disk full"));
        let WorldError::Io { location, source } = &err else {
            panic!("io must build the Io variant");
        };

        assert_eq!(location, "w/region/r.0.0.mca");
        assert_eq!(
            source.downcast_ref::<TestSource>().map(|cause| cause.0),
            Some("disk full")
        );
        assert_eq!(
            err.to_string(),
            "file I/O failed for w/region/r.0.0.mca: disk full"
        );
        assert_eq!(
            err.source().map(ToString::to_string).as_deref(),
            Some("disk full")
        );
    }

    #[test]
    fn parse_failures_chain_the_anvil_error() {
        let located = WorldError::region_file("w/region/r.0.0.mca", parse_failure());
        assert_eq!(
            located.to_string(),
            format!(
                "failed to process region file w/region/r.0.0.mca: {}",
                parse_failure()
            )
        );
        let unlocated = WorldError::from(parse_failure());
        assert_eq!(
            unlocated.to_string(),
            format!("region image error: {}", parse_failure())
        );

        for err in [located, unlocated] {
            assert_eq!(
                err.source()
                    .and_then(|source| source.downcast_ref::<AnvilError>()),
                Some(&parse_failure()),
                "{err}"
            );
        }
    }

    #[test]
    fn layout_and_lock_failures_name_their_subject_without_a_source() {
        let cases = [
            (
                WorldError::Locked {
                    location: "w/.sekai.lock".to_owned(),
                },
                "world is in use by another sekai operation (lock w/.sekai.lock): wait for it to finish, or stop the operation holding it, then retry".to_owned(),
            ),
            (
                WorldError::UnknownRegionPath { key: KEY },
                format!(
                    "cannot derive region path for dim 4242, kind {}, r.-1.2",
                    RegionKind::Region.raw()
                ),
            ),
            (
                WorldError::AmbiguousRegion {
                    key: KEY,
                    first: "w/r.-1.2.mca".to_owned(),
                    second: "w/r.-01.02.mca".to_owned(),
                },
                format!(
                    "two region files spell the same region (-1,2) of dim 4242, kind {}: w/r.-1.2.mca and w/r.-01.02.mca",
                    RegionKind::Region.raw()
                ),
            ),
        ];

        for (err, message) in cases {
            assert_eq!(err.to_string(), message);
            assert!(err.source().is_none(), "{err}");
        }
    }
}
