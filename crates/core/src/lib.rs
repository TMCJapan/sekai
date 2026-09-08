#![no_std]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

extern crate alloc;

pub mod domain;
pub mod port;
pub mod usecase;

pub use domain::{
    ApplyOutcome, BlobHash, ChunkCoord, ChunkHistoryEntry, CoreError, DiffHash, Dimension, GcPlan,
    RegionFingerprint, RegionKey, RegionKind, RegionStateEntry, Snapshot, SnapshotEntry,
    SnapshotId,
};
pub use port::{
    BlobHasher, BlobStore, DiffHasher, MetaStore, Normalizer, RawChunk, RegionReader, RegionWriter,
};
