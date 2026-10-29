#![no_std]
extern crate alloc;

mod chunk;
mod diff;

pub use chunk::{BlockEntity, BlockTick, ChunkPayload, FluidTick, Heightmaps, Section};
pub use diff::ChunkPayloadDiff;
pub use nanonbt;
pub use nanonbt::Error as NbtError;
use sekai_util::DiffHash;

/// Deserializes a level-chunk NBT root into [`ChunkPayload`].
pub fn parse(bytes: &[u8]) -> Result<ChunkPayload<'_>, NbtError> {
    nanonbt::from_slice(bytes)
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
