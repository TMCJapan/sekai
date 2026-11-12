//! Backend-neutral storage boundaries.
//!
//! Concrete database backends live in sibling crates (for example
//! `sekai-storage-local`) and implement the traits exported here.

/// Async `BlobStore`/`MetaStore` traits, shared errors, and the store handle.
pub mod api;
/// High-level store traits implemented by concrete backends.
pub mod store;

pub use api::{BlobStore, MetaStore, StorageError, Store, io_error};
pub use store::{SekaiStore, SekaiStoreOpener};
