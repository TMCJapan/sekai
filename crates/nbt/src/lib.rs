#![no_std]
extern crate alloc;

mod chunk;
mod diff;

pub use chunk::legacy;
pub use chunk::{
    BlockEntity, BlockTick, Chunk, ChunkPayload, FluidTick, Heightmaps, LegacyChunkPayload,
    LegacySection, RawCompound, Section,
};
pub use diff::ChunkPayloadDiff;
pub use nanonbt;
pub use nanonbt::Error as NbtError;
use sekai_util::DiffHash;

/// Deserializes a level-chunk NBT root, either format.
///
/// The 1.13+ shape is tried first: it is the common case, and trying it
/// costs nothing extra there. A legacy chunk is exactly the case where that
/// read fails and the pre-1.13 `Level` wrapper decodes instead. Errors are
/// reported from whichever schema matched, so a malformed chunk still fails
/// loudly.
pub fn parse(bytes: &[u8]) -> Result<Chunk<'_>, NbtError> {
    match nanonbt::from_slice(bytes) {
        Ok(modern) => Ok(Chunk::Modern(modern)),
        Err(_modern_error) => match nanonbt::from_slice(bytes) {
            Ok(legacy) => Ok(Chunk::Legacy(legacy)),
            Err(legacy_error) => Err(legacy_error),
        },
    }
}

/// Hashes a level-chunk NBT root through its normalized view.
///
/// The normalized view excludes the volatile tags `InhabitedTime` and
/// `LastUpdate`; the resulting hash is intended for change detection, not
/// storage identity.
pub fn diff_hash(raw_nbt: &[u8]) -> Result<DiffHash, NbtError> {
    let value = parse(raw_nbt)?;
    value.normalize_hash()
}

/// Hashes a level-chunk NBT root through its normalized view using the V1
/// rules.
///
/// Retained for API compatibility: identical to [`diff_hash`], which keeps
/// the V1 volatile-tag exclusions.
pub fn diff_hash_v1(raw_nbt: &[u8]) -> Result<DiffHash, NbtError> {
    diff_hash(raw_nbt)
}

#[cfg(test)]
mod tests {
    use nanonbt::Cesu8;

    use super::*;

    #[test]
    fn normalized_hash_is_deterministic_and_skips_volatile_tags() {
        let status = Cesu8::try_from_str("minecraft:full").unwrap();
        let payload = |inhabited_time, last_update| ChunkPayload {
            data_version: 1,
            x_pos: 3,
            y_pos: 0,
            z_pos: -1,
            inhabited_time,
            last_update,
            sections: alloc::vec![],
            heightmaps: Heightmaps::default(),
            status,
            block_entities: alloc::vec![],
            block_ticks: alloc::vec![],
            fluid_ticks: alloc::vec![],
        };

        let base = payload(7, 9);
        let base_hash = base.normalize_hash().unwrap();
        assert_eq!(base_hash, payload(7, 9).normalize_hash().unwrap());
        assert_eq!(base_hash, payload(999, 1_234).normalize_hash().unwrap());
    }

    #[test]
    fn volatile_only_change_reports_nothing() {
        let status = Cesu8::try_from_str("minecraft:full").unwrap();
        let payload = |inhabited_time, last_update| ChunkPayload {
            data_version: 1,
            x_pos: 3,
            y_pos: 0,
            z_pos: -1,
            inhabited_time,
            last_update,
            sections: alloc::vec![],
            heightmaps: Heightmaps::default(),
            status,
            block_entities: alloc::vec![],
            block_ticks: alloc::vec![],
            fluid_ticks: alloc::vec![],
        };

        let old = Chunk::Modern(payload(7, 9));
        let new = Chunk::Modern(payload(999, 1_234));
        assert_eq!(
            ChunkPayloadDiff::new(&old, &new).unwrap(),
            ChunkPayloadDiff::Nothing
        );
    }

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
            heightmaps: Heightmaps::default(),
            status: &Cesu8::from_str("minecraft:full"),
            block_entities: alloc::vec![],
            block_ticks: alloc::vec![],
            fluid_ticks: alloc::vec![],
        };
        let bytes = nanonbt::to_bytes(&payload).unwrap();
        let parsed = nanonbt::from_slice::<ChunkPayload>(&bytes).unwrap();
        assert_eq!(payload, parsed);
    }

    #[test]
    fn section_reads_vanilla_light_keys_and_defaults_missing_fields() {
        use nanonbt::SerializeNbt;

        #[derive(SerializeNbt)]
        struct MinimalSection {
            #[nbt(rename = "Y")]
            y: i8,
        }

        let bytes = nanonbt::to_bytes(&MinimalSection { y: 3 }).unwrap();
        let section: Section = nanonbt::from_slice(&bytes).unwrap();
        assert_eq!(section.y, 3);
        assert_eq!(section.block_lights, []);
        assert_eq!(section.sky_lights, []);

        let section = Section {
            y: -2,
            block_lights: &[1, 2, 3],
            sky_lights: &[4, 5],
            ..Section::default()
        };
        let bytes = nanonbt::to_bytes(&section).unwrap();
        assert!(bytes.windows(10).any(|window| window == b"BlockLight"));
        assert!(bytes.windows(8).any(|window| window == b"SkyLight"));
        assert!(!bytes.windows(11).any(|window| window == b"BlockLights"));

        let parsed: Section = nanonbt::from_slice(&bytes).unwrap();
        assert_eq!(parsed, section);
    }

    #[test]
    fn normalized_hash_covers_both_tick_lists() {
        let id = Cesu8::try_from_str("minecraft:water").unwrap();
        let status = Cesu8::try_from_str("minecraft:full").unwrap();
        let tick = BlockTick {
            x: 1,
            y: 2,
            z: 3,
            tick: 4,
            priority: 5,
            id,
        };
        let payload = |block_ticks, fluid_ticks| ChunkPayload {
            data_version: 1,
            x_pos: 3,
            y_pos: 0,
            z_pos: -1,
            inhabited_time: 7,
            last_update: 9,
            sections: alloc::vec![],
            heightmaps: Heightmaps::default(),
            status,
            block_entities: alloc::vec![],
            block_ticks,
            fluid_ticks,
        };

        let empty = payload(alloc::vec![], alloc::vec![]);
        let block = payload(alloc::vec![tick.clone()], alloc::vec![]);
        let fluid = payload(alloc::vec![], alloc::vec![tick]);

        assert_ne!(
            empty.normalize_hash().unwrap(),
            block.normalize_hash().unwrap()
        );
        assert_ne!(
            empty.normalize_hash().unwrap(),
            fluid.normalize_hash().unwrap()
        );
    }
}
