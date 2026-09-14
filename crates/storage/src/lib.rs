//! Content-addressed blob storage and MVCC metadata.
//!
//! Single crate with strict module boundaries mirroring the future split 1:1
//! (`api` / `cas` / `sqlite` / `mysql` / `postgres`). Backend modules are
//! feature-gated; only `backend-sqlite` ships.

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
