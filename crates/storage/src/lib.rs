//! Backend-neutral storage boundaries.
//!
//! Owns the async `BlobStore`/`MetaStore`/`CacheStore` traits, the canonical
//! snapshot-tree representation they exchange, and the shared storage errors.
//! Concrete database backends live in sibling crates (for example
//! `sekai-storage-local`) and implement the traits exported here.
//! The crate is `no_std` + `alloc` so a backend for any target (host,
//! wasm, embedded) implements the same traits and error surface.

#![no_std]

extern crate alloc;

/// Shared errors and the store handle.
pub mod api;
/// Content-addressed blob storage boundary.
pub mod blob;
/// Rebuildable region-image cache boundary.
pub mod cache;
/// Commit metadata and snapshot-tree storage boundary.
pub mod meta;
/// High-level store traits implemented by concrete backends.
pub mod store;
/// Canonical snapshot-tree codec.
pub mod tree;

pub use api::{StorageError, Store, io_error};
pub use blob::BlobStore;
pub use cache::CacheStore;
pub use meta::MetaStore;
pub use store::SekaiStore;
pub use tree::{
    DimEntry, DimNode, NodeLevel, RegionEntry, RegionNode, RootEntry, RootNode, TreeError,
    TreeNodes, empty_tree,
};
