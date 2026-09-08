//! Boundary traits implemented by outer crates.
//!
//! Rationale: `core` owns shapes and ordering guarantees while `mca`,
//! `nbt`, and `storage` own I/O, codecs, and SQLite. Each port covers one
//! capability so concrete implementations stay substitutable without
//! touching use-case logic.

pub mod blob;
pub mod hash;
pub mod meta;
pub mod region;

pub use blob::BlobStore;
pub use hash::{BlobHasher, DiffHasher};
pub use meta::MetaStore;
pub use region::{Normalizer, RawChunk, RegionReader, RegionWriter};
