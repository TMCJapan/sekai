//! Backend-neutral storage boundaries plus the file CAS.
//!
//! Concrete database backends live in sibling crates (for example
//! `sekai-storage-sqlite`) and implement the traits exported here.

/// Async `BlobStore`/`MetaStore` traits, `BackendKind`, URL selection.
pub mod api;
/// File CAS (`blobs/ab/cdef...`).
pub mod cas;
/// Exclusive-run marker (`commit.inflight`) guarding `gc` against commits.
pub mod runs;
/// High-level store traits implemented by concrete backends.
pub mod store;

pub use api::{
    BackendKind, BlobStore, MetaStore, StorageError, Store, io_error, parse_backend_url,
};
pub use cas::FileCas;
pub use runs::RunGuard;
pub use store::{SekaiStore, SekaiStoreOpener};
