//! Storage boundaries implemented by backend adapters.

pub mod blob;
pub mod meta;

pub use blob::BlobStore;
pub use meta::MetaStore;
