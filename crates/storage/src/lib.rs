//! Backend-neutral storage boundaries.
//!
//! Concrete database backends live in sibling crates (for example
//! `sekai-storage-local`) and implement the traits exported here.

/// Async `BlobStore`/`MetaStore` traits, `BackendKind`, URL selection.
pub mod api;
/// High-level store traits implemented by concrete backends.
pub mod store;

pub use api::{
    BackendKind, BlobStore, MetaStore, StorageError, Store, io_error, parse_backend_url,
};
pub use store::{SekaiStore, SekaiStoreOpener};
