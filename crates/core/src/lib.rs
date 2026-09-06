#![no_std]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

extern crate alloc;

pub mod coords;
pub mod error;
pub mod hash;
pub mod history;
pub mod snapshot;
pub mod traits;

pub use coords::{ChunkCoord, Dimension, RegionKind};
pub use error::CoreError;
pub use hash::{BlobHash, DiffHash};
pub use history::ChunkHistoryEntry;
pub use snapshot::{Snapshot, SnapshotId};
pub use traits::{
    BlobHasher, BlobStore, DiffHasher, MetaStore, Normalizer, RawChunk, RegionReader, RegionWriter,
};
