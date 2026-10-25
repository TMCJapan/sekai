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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_round_trips() {
        let payload = ChunkPayload {
            data_version: 1,
            x_pos: 3,
            y_pos: 0,
            z_pos: -1,
            inhabited_time: 7,
            last_update: 9,
            sections: alloc::vec![],
            heightmaps: Heightmaps {},
            status: "minecraft:full".into(),
            block_entities: alloc::vec![],
        };
        let bytes = nanonbt::to_bytes(&payload).unwrap();
        let parsed = nanonbt::from_bytes::<ChunkPayload>(&bytes).unwrap();
        assert_eq!(payload, parsed);
    }
}
