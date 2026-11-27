//! `no_std` Anvil region-image codec.
//!
//! Provides strict `.mca` parsing, chunk decompression and normalized
//! hashing, region-coordinate mapping, and rebuilding of region images.
//!
//! Filesystem access is intentionally outside this crate.

#![no_std]

extern crate alloc;

mod codec;
mod diff;
mod error;
mod hash;
mod nbt;
mod reader;
mod region;
mod writer;

pub use codec::{Compression, decompress_into};
pub use diff::ChunkPayloadDiff;
pub use error::{AnvilError, ChunkHashError, ChunkNbtError};
pub use hash::{custom_dimension_id, custom_dimension_id_bytes};
pub use reader::{Chunk, RegionImage};
pub use region::{
    HEADER_LEN, RegionLoc, TABLE_ENTRIES, check_image_len, parse_region_name, visit_present_coords,
};
pub use writer::RegionBuilder;
