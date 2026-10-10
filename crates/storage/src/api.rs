//! Backend selection, shared errors, and the store handle.
//!
//! URL schemes select compiled-in backends; `Store` keeps metadata and CAS together.

use std::path::{Path, PathBuf};

pub use sekai_core::{BlobStore, MetaStore};

/// Failures while storing blobs or metadata.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("file I/O failed for {path}: {source}", path = path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Requested blob is absent from CAS; this is corruption because tombstones
    /// are represented in metadata.
    #[error("blob missing from CAS: {hex}")]
    BlobMissing { hex: String },
    /// URL names a backend that is unknown or not compiled in.
    #[error("unsupported storage backend: {url}")]
    UnsupportedBackend { url: String },
    /// URL carries no store location.
    #[error("store location is empty: pass a directory path")]
    EmptyStorePath,
    /// The root holds no initialized store. Only backup runs create one;
    /// every other operation must fail here instead of materializing an
    /// empty store as a side effect.
    #[error(
        "no store at {path}: run a backup to create one",
        path = path.display()
    )]
    StoreMissing {
        /// Store root that was expected to hold `meta.sqlite`.
        path: PathBuf,
    },
    /// A writing run holds the store; `gc` must not run alongside it.
    #[error(
        "store is in use by a running backup (marker {path}{holder}): a backup writes blobs before it commits the rows that reference them, so collecting now would unlink blobs the next commit still needs. Wait for it to finish, or remove the marker if no backup is running."
    )]
    StoreBusy {
        /// Marker file that was found.
        path: PathBuf,
        /// Contents recorded by the run holding it.
        holder: String,
    },
    /// Stored hash bytes are not 32 bytes long.
    #[error("stored hash has invalid length: {len} bytes, expected 32")]
    InvalidHashLength { len: usize },
    /// Stored history integer does not fit its domain type.
    #[error("stored history column {column} has out-of-range value: {value}")]
    InvalidHistoryValue { column: &'static str, value: i64 },
    /// Stored dimension registry code does not fit the domain type.
    #[error("stored dimension code is out of range: {value}")]
    InvalidDimensionCode { value: i64 },
    /// Stored tag name fails validation.
    #[error("stored tag name is invalid: {name}")]
    InvalidTagName { name: String },
    /// Millisecond timestamp does not fit `i64`.
    #[error("timestamp out of range: {0}")]
    InvalidTimestamp(u64),
    /// Region file size does not fit `i64`.
    #[error("region size out of range: {0}")]
    InvalidSize(u64),
    /// Snapshot ID does not fit SQLite's signed integer type.
    #[error("snapshot ID out of range: {0}")]
    InvalidSnapshotId(u64),
    /// A retirement must fold a snapshot into a strictly newer one.
    #[error("cannot retire snapshot {from} into {into}: the successor must be newer")]
    InvalidRetirement {
        /// Snapshot being retired.
        from: u64,
        /// Intended successor.
        into: u64,
    },
    /// The named snapshot does not exist.
    #[error("unknown snapshot: {id}")]
    UnknownSnapshot {
        /// Requested snapshot ID.
        id: u64,
    },
    /// Database schema version is not supported.
    #[error(
        "unsupported schema version: {found}, this binary supports {supported} (recreate the store; pre-release stores are not migrated)"
    )]
    UnsupportedSchema { found: i64, supported: i64 },
    /// SQLite operation failed.
    #[cfg(feature = "backend-sqlite")]
    #[error("sqlite failed: {0}")]
    Sqlite(#[from] sqlx::Error),
}

/// Selectable metadata backend. Variants exist only for compiled-in
/// backends; URLs naming anything else fail loudly at open time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// `sqlite://<dir>` (or a bare `<dir>`): metadata lives in
    /// `<dir>/meta.sqlite`, blobs in `<dir>/blobs/`.
    #[cfg(feature = "backend-sqlite")]
    Sqlite,
    /// Reserved stub.
    #[cfg(feature = "backend-mysql")]
    Mysql,
    /// Reserved stub.
    #[cfg(feature = "backend-postgres")]
    Postgres,
}

/// Split a store URL into backend and the part after `://`.
///
/// Bare paths select SQLite when that backend is compiled; unknown schemes
/// fail. An empty location fails too: it would resolve against the process
/// working directory and scatter a store there.
pub fn parse_backend_url(url: &str) -> Result<(BackendKind, &str), StorageError> {
    let unsupported = || StorageError::UnsupportedBackend {
        url: url.to_owned(),
    };
    let selected = match url.split_once("://") {
        #[cfg(feature = "backend-sqlite")]
        Some(("sqlite", rest)) => Some((BackendKind::Sqlite, rest)),
        #[cfg(feature = "backend-mysql")]
        Some(("mysql", rest)) => Some((BackendKind::Mysql, rest)),
        #[cfg(feature = "backend-postgres")]
        Some(("postgres", rest)) => Some((BackendKind::Postgres, rest)),
        Some(_) => None,
        None => {
            #[cfg(feature = "backend-sqlite")]
            {
                Some((BackendKind::Sqlite, url))
            }
            #[cfg(not(feature = "backend-sqlite"))]
            {
                None
            }
        }
    };
    match selected {
        Some((_, "")) => Err(StorageError::EmptyStorePath),
        Some(selection) => Ok(selection),
        None => Err(unsupported()),
    }
}

/// Metadata and CAS adapters owned by one store handle.
#[derive(Debug, Clone)]
pub struct Store<M, C> {
    meta: M,
    cas: C,
}

impl<M, C> Store<M, C> {
    /// Compose an opened store from its adapters.
    pub const fn new(meta: M, cas: C) -> Self {
        Self { meta, cas }
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
    pub const fn cas(&self) -> &C {
        &self.cas
    }

    /// Mutable blob adapter.
    pub const fn cas_mut(&mut self) -> &mut C {
        &mut self.cas
    }

    /// Borrow both adapters for operations such as GC that need simultaneous access.
    pub const fn cas_and_meta(&mut self) -> (&mut C, &M) {
        (&mut self.cas, &self.meta)
    }
}

/// Wrap an I/O failure together with the path that caused it.
pub(crate) fn io_error(path: impl AsRef<Path>, source: std::io::Error) -> StorageError {
    StorageError::Io {
        path: path.as_ref().to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_schemes_are_unsupported_without_compiled_backends() {
        assert!(matches!(
            parse_backend_url("s3://bucket/store"),
            Err(StorageError::UnsupportedBackend { .. })
        ));
    }

    #[cfg(feature = "backend-sqlite")]
    #[test]
    fn parses_sqlite_urls_and_bare_dirs() {
        assert_eq!(
            parse_backend_url("sqlite:///var/sekai").unwrap(),
            (BackendKind::Sqlite, "/var/sekai")
        );
        assert_eq!(
            parse_backend_url("/var/sekai").unwrap(),
            (BackendKind::Sqlite, "/var/sekai")
        );
        assert!(matches!(
            parse_backend_url("s3://bucket/store"),
            Err(StorageError::UnsupportedBackend { .. })
        ));
    }

    #[cfg(feature = "backend-sqlite")]
    #[test]
    fn empty_store_locations_are_rejected() {
        assert!(matches!(
            parse_backend_url(""),
            Err(StorageError::EmptyStorePath)
        ));
        assert!(matches!(
            parse_backend_url("sqlite://"),
            Err(StorageError::EmptyStorePath)
        ));
    }
}
