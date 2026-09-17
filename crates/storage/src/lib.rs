//! Content-addressed blob storage and MVCC metadata.
//!
//! Backend modules are feature-gated; SQLite is the implemented backend.

/// Async `BlobStore`/`MetaStore` traits, `BackendKind`, URL selection.
pub mod api;
/// File CAS (`blobs/ab/cdef...`).
pub mod cas;
/// Reserved MySQL backend stub.
#[cfg(feature = "backend-mysql")]
pub mod mysql;
/// Reserved Postgres backend stub.
#[cfg(feature = "backend-postgres")]
pub mod postgres;
/// SQLite backend (sqlx).
#[cfg(feature = "backend-sqlite")]
pub mod sqlite;

pub use api::{BackendKind, BlobStore, MetaStore, StorageError, Store, parse_backend_url};
pub use cas::FileCas;
#[cfg(feature = "backend-sqlite")]
pub use sqlite::{SqliteMeta, SqliteStore, open_sqlite};
