//! Backend-neutral storage boundaries.
//!
//! Concrete database backends live in sibling crates (for example
//! `sekai-storage-local`) and implement the traits exported here.
//! The crate is `no_std` + `alloc` so a backend for any target (host,
//! wasm, embedded) implements the same traits and error surface.

#![no_std]

extern crate alloc;

/// Async `BlobStore`/`MetaStore` traits, shared errors, and the store handle.
pub mod api;
/// High-level store traits implemented by concrete backends.
pub mod store;

pub use api::{BlobStore, MetaStore, StorageError, Store, io_error};
pub use store::{SekaiStore, SekaiStoreOpener};
