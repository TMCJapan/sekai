//! Shared errors and the store handle.
//!
//! `Store` keeps a backend's metadata and blob adapters together.

use alloc::boxed::Box;
use alloc::string::String;
use core::fmt;

pub use sekai_core::{BlobStore, CommitId, MetaStore};

/// Failures while storing blobs or metadata.
///
/// Paths are plain strings and sources are boxed [`core::error::Error`]s so
/// every backend shares one error surface, whether it runs on a host
/// filesystem or a `no_std` target.
#[derive(Debug)]
pub enum StorageError {
    /// An I/O operation failed at `path`.
    Io {
        /// Location the failing operation targeted.
        path: String,
        /// Underlying I/O failure.
        source: Box<dyn core::error::Error + Send + Sync>,
    },
    /// Requested blob is absent from CAS; this is corruption because tombstones
    /// are represented in metadata.
    BlobMissing {
        /// Hex-encoded hash of the absent blob.
        hex: String,
    },
    /// The root holds no initialized store. Only an explicit init creates
    /// one; every other operation must fail here instead of materializing
    /// an empty store as a side effect.
    StoreMissing {
        /// Store root that was expected to hold `meta.sqlite` and the `blobs` keyspace.
        path: String,
    },
    /// Stored hash bytes are not 32 bytes long.
    InvalidHashLength {
        /// Byte length read from storage.
        len: usize,
    },
    /// Stored history integer does not fit its domain type.
    InvalidHistoryValue {
        /// Column that carried the out-of-range value.
        column: &'static str,
        /// Value read from storage.
        value: i64,
    },
    /// Stored ref name fails validation.
    InvalidRefName {
        /// Ref name that failed validation.
        name: String,
    },
    /// Stored scope name fails validation.
    InvalidScopeName {
        /// Scope name that failed validation.
        name: String,
    },
    /// Stored scope has no area: `NamedScope::new` refuses an empty area
    /// set, so `set_scope` never writes one and a scope without area rows
    /// is corruption.
    EmptyScope {
        /// Scope that has no area.
        name: String,
    },
    /// The named ref does not exist.
    ///
    /// Emitted by operations that require the ref to exist
    /// (`activate_ref`); lookups and `delete_ref` report absence as
    /// `None`/`false` instead.
    UnknownRef {
        /// Requested ref name.
        name: String,
    },
    /// The active ref cannot be removed: a store without one would have
    /// nowhere to hang new commits from.
    ActiveRefProtected {
        /// Ref that was asked for.
        name: String,
    },
    /// The store has no active ref, so a new commit has nowhere to land.
    ///
    /// Field-less on purpose: the condition is store-wide, not tied to a
    /// name the caller passed. A backend checks for the active ref before
    /// its parent constraint, so `NoActiveRef` takes priority over
    /// [`Self::NotContinuing`] when both conditions hold.
    NoActiveRef,
    /// A new commit does not continue the active ref's target, which
    /// would fork the ref onto another history.
    NotContinuing {
        /// Target commit ID the new commit had to include as a parent.
        base: CommitId,
    },
    /// Stored region-family code does not name a known kind.
    UnknownRegionKind {
        /// Code read from the `kind` column.
        kind: i32,
    },
    /// Millisecond timestamp does not fit `i64`.
    InvalidTimestamp(u64),
    /// The system clock is before the Unix epoch.
    ///
    /// Raised when a millisecond reading of the system clock cannot be
    /// represented as a non-negative `u64` timestamp.
    ClockBeforeEpoch,
    /// The system clock is past the representable range.
    ///
    /// Raised when a millisecond reading of the system clock does not fit
    /// the timestamp range the backend stores.
    ClockOutOfRange,
    /// Region file size does not fit `i64`.
    InvalidSize(u64),
    /// The named commit does not exist.
    UnknownCommit {
        /// Requested commit ID.
        id: CommitId,
    },
    /// Database schema version is not supported.
    UnsupportedSchema {
        /// Version found in the store.
        found: i64,
        /// Version this binary supports.
        supported: i64,
    },
    /// A backend-specific operation failed.
    Backend {
        /// Backend name used in diagnostics.
        backend: &'static str,
        /// Underlying backend failure.
        source: Box<dyn core::error::Error + Send + Sync>,
    },
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "file I/O failed for {path}: {source}"),
            Self::BlobMissing { hex } => write!(f, "blob missing from CAS: {hex}"),
            Self::StoreMissing { path } => {
                write!(f, "no store at {path}: run 'sekai init' to create one")
            }
            Self::InvalidHashLength { len } => {
                write!(
                    f,
                    "stored hash has invalid length: {len} bytes, expected 32"
                )
            }
            Self::InvalidHistoryValue { column, value } => write!(
                f,
                "stored history column {column} has out-of-range value: {value}"
            ),
            Self::InvalidRefName { name } => write!(f, "stored ref name is invalid: {name}"),
            Self::InvalidScopeName { name } => write!(f, "stored scope name is invalid: {name}"),
            Self::EmptyScope { name } => write!(f, "stored scope has no area: {name}"),
            Self::UnknownRef { name } => write!(f, "unknown ref: {name}"),
            Self::ActiveRefProtected { name } => {
                write!(f, "cannot remove the active ref: {name}")
            }
            Self::NoActiveRef => {
                f.write_str("store has no active ref: a new commit has nowhere to hang from")
            }
            Self::NotContinuing { base } => {
                write!(
                    f,
                    "new commit does not continue the active ref's target {base}"
                )
            }
            Self::UnknownRegionKind { kind } => write!(f, "stored region kind is unknown: {kind}"),
            Self::InvalidTimestamp(millis) => write!(f, "timestamp out of range: {millis}"),
            Self::ClockBeforeEpoch => f.write_str("system clock is before the Unix epoch"),
            Self::ClockOutOfRange => f.write_str("system clock is out of range"),
            Self::InvalidSize(size) => write!(f, "region size out of range: {size}"),
            Self::UnknownCommit { id } => write!(f, "unknown commit: {id}"),
            Self::UnsupportedSchema { found, supported } => write!(
                f,
                "unsupported schema version: {found}, this binary supports {supported} (recreate the store; pre-release stores are not migrated)"
            ),
            Self::Backend { backend, source } => write!(f, "{backend} failed: {source}"),
        }
    }
}

impl core::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } | Self::Backend { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

impl StorageError {
    /// Wrap a backend-specific failure without coupling this crate to that
    /// backend's error type.
    pub fn backend(
        backend: &'static str,
        source: impl core::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Backend {
            backend,
            source: Box::new(source),
        }
    }
}

/// Metadata and blob adapters owned by one store handle.
#[derive(Debug, Clone)]
pub struct Store<M, C> {
    meta: M,
    blob: C,
}

impl<M, C> Store<M, C> {
    /// Compose an opened store from its adapters.
    pub const fn new(meta: M, blob: C) -> Self {
        Self { meta, blob }
    }

    /// Metadata adapter.
    pub const fn meta(&self) -> &M {
        &self.meta
    }

    /// Mutable metadata adapter.
    pub const fn meta_mut(&mut self) -> &mut M {
        &mut self.meta
    }

    /// Blob adapter.
    pub const fn blob(&self) -> &C {
        &self.blob
    }

    /// Mutable blob adapter.
    pub const fn blob_mut(&mut self) -> &mut C {
        &mut self.blob
    }
}

/// Wrap an I/O failure together with the path that caused it.
///
/// Backend crates build [`StorageError::Io`] through this so the error
/// construction stays in one place. The path is a display string: the
/// boundary never interprets it, only the backend does.
pub fn io_error(
    path: impl Into<String>,
    source: impl core::error::Error + Send + Sync + 'static,
) -> StorageError {
    StorageError::Io {
        path: path.into(),
        source: Box::new(source),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::borrow::ToOwned;
    use alloc::string::ToString;
    use core::error::Error as _;

    /// Stand-in backend failure: the boundary only needs `Display` plus
    /// `Error`, never a concrete host I/O type.
    #[derive(Debug)]
    struct TestSource(&'static str);

    impl fmt::Display for TestSource {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.0)
        }
    }

    impl core::error::Error for TestSource {}

    #[test]
    fn io_error_keeps_path_and_source() {
        let err = io_error("store/meta.sqlite", TestSource("disk full"));
        let StorageError::Io { path, source } = &err else {
            panic!("io_error must build the Io variant");
        };

        assert_eq!(path, "store/meta.sqlite");
        assert_eq!(source.to_string(), "disk full");
        assert_eq!(
            err.to_string(),
            "file I/O failed for store/meta.sqlite: disk full"
        );
    }

    #[test]
    fn backend_error_carries_name_and_source() {
        let err = StorageError::backend("sled", TestSource("corrupt tree"));

        assert_eq!(err.to_string(), "sled failed: corrupt tree");
        assert_eq!(
            err.source().map(ToString::to_string).as_deref(),
            Some("corrupt tree")
        );
    }

    #[test]
    fn store_exposes_both_adapters() {
        let mut store = Store::new("meta".to_owned(), "blob".to_owned());

        assert_eq!(store.meta().as_str(), "meta");
        assert_eq!(store.blob().as_str(), "blob");

        *store.meta_mut() = "new meta".to_owned();
        *store.blob_mut() = "new blob".to_owned();
    }
}
