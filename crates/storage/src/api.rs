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
    /// Stored hash bytes are not 32 bytes long.
    #[error("stored hash has invalid length: {len} bytes, expected 32")]
    InvalidHashLength { len: usize },
    /// Stored history integer does not fit its domain type.
    #[error("stored history column {column} has out-of-range value: {value}")]
    InvalidHistoryValue { column: &'static str, value: i64 },
    /// Millisecond timestamp does not fit `i64`.
    #[error("timestamp out of range: {0}")]
    InvalidTimestamp(u64),
    /// Snapshot ID does not fit SQLite's signed integer type.
    #[error("snapshot ID out of range: {0}")]
    InvalidSnapshotId(u64),
    /// Database schema version is not supported.
    #[error("unsupported schema version: {found}, this binary supports {supported}")]
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
/// Bare paths select SQLite when that backend is compiled; unknown schemes fail.
pub fn parse_backend_url(url: &str) -> Result<(BackendKind, &str), StorageError> {
    let unsupported = || StorageError::UnsupportedBackend {
        url: url.to_owned(),
    };
    match url.split_once("://") {
        #[cfg(feature = "backend-sqlite")]
        Some(("sqlite", rest)) => Ok((BackendKind::Sqlite, rest)),
        #[cfg(feature = "backend-mysql")]
        Some(("mysql", rest)) => Ok((BackendKind::Mysql, rest)),
        #[cfg(feature = "backend-postgres")]
        Some(("postgres", rest)) => Ok((BackendKind::Postgres, rest)),
        Some(_) => Err(unsupported()),
        None => {
            #[cfg(feature = "backend-sqlite")]
            {
                Ok((BackendKind::Sqlite, url))
            }
            #[cfg(not(feature = "backend-sqlite"))]
            {
                Err(unsupported())
            }
        }
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

/// Store root as a directory path.
pub(crate) fn io_error(path: &Path, source: std::io::Error) -> StorageError {
    StorageError::Io {
        path: path.to_path_buf(),
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
}
