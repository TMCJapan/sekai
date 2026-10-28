//! Backend selection, shared errors, and the store handle.
//!
//! URL schemes select compiled-in backends; `Store` keeps metadata and CAS together.

use std::path::{Path, PathBuf};

pub use sekai_core::{BlobStore, CommitId, MetaStore};

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
    /// URL names a backend this build cannot serve.
    #[error("unsupported storage backend: {url}")]
    UnsupportedBackend { url: String },
    /// URL carries no store location.
    #[error("store location is empty: pass a directory path")]
    EmptyStorePath,
    /// The root holds no initialized store. Only commit runs create one;
    /// every other operation must fail here instead of materializing an
    /// empty store as a side effect.
    #[error(
        "no store at {path}: run a commit to create one",
        path = path.display()
    )]
    StoreMissing {
        /// Store root that was expected to hold `meta.sqlite`.
        path: PathBuf,
    },
    /// A writing run holds the store; `gc` must not run alongside it.
    #[error(
        "store is in use by a running commit (marker {path}{holder}): a commit writes blobs before it commits the rows that reference them, so collecting now would unlink blobs the next commit still needs. Wait for it to finish, or remove the marker if no commit is running.",
        path = path.display()
    )]
    StoreBusy {
        /// Marker file that was found.
        path: PathBuf,
        /// Contents recorded by the run holding it.
        holder: String,
    },
    /// Another operation holds a conflicting lock on the store.
    #[error(
        "store is in use by another sekai operation (lock {path}): retry once it finishes",
        path = path.display()
    )]
    StoreLocked {
        /// Lock file that could not be taken.
        path: PathBuf,
    },
    /// Stored hash bytes are not 32 bytes long.
    #[error("stored hash has invalid length: {len} bytes, expected 32")]
    InvalidHashLength { len: usize },
    /// Stored history integer does not fit its domain type.
    #[error("stored history column {column} has out-of-range value: {value}")]
    InvalidHistoryValue { column: &'static str, value: i64 },
    /// Stored ref name fails validation.
    #[error("stored ref name is invalid: {name}")]
    InvalidRefName {
        /// Ref name that failed validation.
        name: String,
    },
    /// Stored scope name fails validation.
    #[error("stored scope name is invalid: {name}")]
    InvalidScopeName {
        /// Scope name that failed validation.
        name: String,
    },
    /// Stored scope has no area: `NamedScope::new` refuses an empty area
    /// set, so `set_scope` never writes one and a scope without area rows
    /// is corruption.
    #[error("stored scope has no area: {name}")]
    EmptyScope {
        /// Scope that has no area.
        name: String,
    },
    /// The named ref does not exist.
    ///
    /// Emitted by operations that require the ref to exist
    /// (`activate_ref`); lookups and `delete_ref` report absence as
    /// `None`/`false` instead.
    #[error("unknown ref: {name}")]
    UnknownRef {
        /// Requested ref name.
        name: String,
    },
    /// The active ref cannot be removed: a store without one would have
    /// nowhere to hang new commits from.
    #[error("cannot remove the active ref: {name}")]
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
    #[error("store has no active ref: a new commit has nowhere to hang from")]
    NoActiveRef,
    /// A new commit does not continue the active ref's target, which
    /// would fork the ref onto another history.
    #[error("new commit does not continue the active ref's target {base}")]
    NotContinuing {
        /// Target commit ID the new commit had to include as a parent.
        base: CommitId,
    },
    /// Stored region-family code does not name a known kind.
    #[error("stored region kind is unknown: {kind}")]
    UnknownRegionKind {
        /// Code read from the `kind` column.
        kind: i32,
    },
    /// Millisecond timestamp does not fit `i64`.
    #[error("timestamp out of range: {0}")]
    InvalidTimestamp(u64),
    /// The system clock is before the Unix epoch.
    ///
    /// Raised when a millisecond reading of the system clock cannot be
    /// represented as a non-negative `u64` timestamp.
    #[error("system clock is before the Unix epoch")]
    ClockBeforeEpoch,
    /// The system clock is past the representable range.
    ///
    /// Raised when a millisecond reading of the system clock does not fit
    /// the timestamp range the backend stores.
    #[error("system clock is out of range")]
    ClockOutOfRange,
    /// Region file size does not fit `i64`.
    #[error("region size out of range: {0}")]
    InvalidSize(u64),
    /// A retirement must fold a commit into a strictly newer one.
    #[error("cannot retire commit {from} into {into}: the successor must be newer")]
    InvalidRetirement {
        /// Commit being retired.
        from: CommitId,
        /// Intended successor.
        into: CommitId,
    },
    /// A retirement must fold a commit into one of its descendants.
    #[error("cannot retire commit {from} into {into}: the successor must descend from it")]
    UnrelatedRetirement {
        /// Commit being retired.
        from: CommitId,
        /// Intended successor.
        into: CommitId,
    },
    /// The named commit does not exist.
    #[error("unknown commit: {id}")]
    UnknownCommit {
        /// Requested commit ID.
        id: CommitId,
    },
    /// Database schema version is not supported.
    #[error(
        "unsupported schema version: {found}, this binary supports {supported} (recreate the store; pre-release stores are not migrated)"
    )]
    UnsupportedSchema { found: i64, supported: i64 },
    /// A backend-specific operation failed.
    #[error("{backend} failed: {source}")]
    Backend {
        /// Backend name used in diagnostics.
        backend: &'static str,
        /// Underlying backend failure.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

impl StorageError {
    /// Wrap a backend-specific failure without coupling this crate to that
    /// backend's error type.
    pub fn backend(
        backend: &'static str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Backend {
            backend,
            source: Box::new(source),
        }
    }
}

/// Selectable metadata backend. Variants exist only for compiled-in
/// backends; URLs naming anything else fail loudly at open time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// `sqlite://<dir>` (or a bare `<dir>`): metadata lives in
    /// `<dir>/meta.sqlite`, blobs in `<dir>/blobs/`.
    Sqlite,
    /// Reserved stub.
    Mysql,
    /// Reserved stub.
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
        Some(("sqlite", rest)) => Some((BackendKind::Sqlite, rest)),
        Some(("mysql", rest)) => Some((BackendKind::Mysql, rest)),
        Some(("postgres", rest)) => Some((BackendKind::Postgres, rest)),
        Some(_) => None,
        None => Some((BackendKind::Sqlite, url)),
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
///
/// Backend crates build [`StorageError::Io`] through this so the path
/// handling stays in one place.
pub fn io_error(path: impl AsRef<Path>, source: std::io::Error) -> StorageError {
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
