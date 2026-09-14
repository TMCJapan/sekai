//! Parse and hash already-decompressed NBT data.
//!
//! Chunk decompression is handled by `sekai-anvil`; this crate owns only NBT
//! parsing and canonical diff hashing.

#![no_std]

extern crate alloc;

mod error;
mod normalize;
mod parser;

pub use error::NbtError;
pub use normalize::DEFAULT_IGNORED;
pub use parser::Value;

/// Parses a complete NBT document with a compound root.
pub fn parse(raw_nbt: &[u8]) -> Result<Value, NbtError> {
    parser::parse_root(raw_nbt)
}

/// Hashes NBT after excluding the specified tag names at every depth.
///
/// The resulting hash is intended for change detection, not storage identity.
pub fn diff_hash(raw_nbt: &[u8], ignore: &[&str]) -> Result<[u8; 32], NbtError> {
    let value = parse(raw_nbt)?;
    Ok(normalize::digest(ignore, &value))
}

/// Hashes NBT using the default volatile-tag ignore set.
pub fn diff_hash_v1(raw_nbt: &[u8]) -> Result<[u8; 32], NbtError> {
    diff_hash(raw_nbt, DEFAULT_IGNORED)
}
