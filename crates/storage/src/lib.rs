//! Backend-neutral storage boundaries.
//!
//! Owns the async `BlobStore`/`MetaStore`/`CacheStore` traits, the canonical
//! snapshot-tree representation they exchange, the generic tree/graph
//! algorithms over those traits, and the shared storage errors.
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
/// Commit-graph reads generic over [`MetaStore`].
pub mod graph;
/// Commit metadata and snapshot-tree storage boundary.
pub mod meta;
/// Ref reads generic over [`MetaStore`].
pub mod refs;
/// Scope reads generic over [`MetaStore`].
pub mod scopes;
/// High-level store traits implemented by concrete backends.
pub mod store;
/// In-memory [`MetaStore`] fake for unit tests (`testing` feature).
#[cfg(any(test, feature = "testing"))]
pub mod testing;
/// Canonical snapshot-tree codec and generic tree operations.
pub mod tree;

pub use api::{StorageError, Store, io_error};
pub use blob::BlobStore;
pub use cache::CacheStore;
pub use graph::{CommitError, CommitStats, ResolveError};
pub use meta::{MetaStore, commit_hash};
pub use store::{SekaiStore, SekaiStoreOpener};
pub use tree::{
    DiffStats, DimEntry, DimNode, NodeLevel, RegionEntry, RegionNode, RootEntry, RootNode,
    TreeError, TreeNodes, empty_tree, file_region_hash,
};
