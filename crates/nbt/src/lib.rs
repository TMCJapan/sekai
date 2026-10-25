#![no_std]
extern crate alloc;

mod chunk;
mod diff;

pub use chunk::ChunkPayload;
pub use chunk::Heightmaps;
pub use diff::ChunkPayloadDiff;
pub use nanonbt;
pub use nanonbt::Error as NbtError;
use sekai_util::DiffHash;

/// Parses a complete NBT document with a compound root.
pub fn parse(bytes: &[u8]) -> Result<ChunkPayload, NbtError> {
    nanonbt::from_bytes(bytes)
}

/// Hashes NBT after excluding the specified tag names at every depth.
///
/// The resulting hash is intended for change detection, not storage identity.
pub fn diff_hash(raw_nbt: &[u8]) -> Result<DiffHash, NbtError> {
    let value = parse(raw_nbt)?;
    value.normalize_hash()
}

/// Hashes NBT using the default volatile-tag ignore set.
pub fn diff_hash_v1(raw_nbt: &[u8]) -> Result<DiffHash, NbtError> {
    diff_hash(raw_nbt)
}
