//! `no_std` Anvil region-image codec.
//!
//! Provides strict `.mca` parsing, chunk decompression, region-coordinate
//! mapping, hashing helpers, and rebuilding of region images.
//!
//! Filesystem access is intentionally outside this crate.

#![no_std]

extern crate alloc;

mod codec;
mod error;
mod hash;
mod reader;
mod region;
mod writer;

pub use codec::{Compression, compression_of, decompress_into};
pub use error::AnvilError;
pub use hash::{custom_dimension_id, header_hash};
pub use reader::{Chunk, RegionImage};
pub use region::{
    FIRST_DATA_SECTOR, HEADER_LEN, MAX_SECTOR_OFFSET, MAX_SECTORS_PER_CHUNK, ROW_WIDTH, RegionLoc,
    SECTOR_LEN, TABLE_ENTRIES, base_coords, check_image_len, parse_region_name, sectors_for,
};
pub use writer::RegionBuilder;
