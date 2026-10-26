mod block_entity;
mod hightmap;
mod section;

pub use block_entity::BlockEntity;
pub use hightmap::Heightmaps;
pub use section::Section;

use alloc::vec::Vec;
use nanonbt::{Cesu8, DeserializeNbt, SerializeNbt};
use sekai_util::DiffHash;

extern crate alloc;

#[derive(SerializeNbt, DeserializeNbt, Debug, Clone, Eq, PartialEq)]
pub struct ChunkPayload<'de> {
    #[nbt(rename = "DataVersion")]
    pub data_version: i32,

    #[nbt(rename = "xPos")]
    pub x_pos: i32,
    #[nbt(rename = "yPos")]
    pub y_pos: i32,
    #[nbt(rename = "zPos")]
    pub z_pos: i32,

    #[nbt(rename = "InhabitedTime")]
    pub inhabited_time: i64,
    #[nbt(rename = "LastUpdate")]
    pub last_update: i64,

    pub sections: Vec<Section<'de>>,

    #[nbt(rename = "Heightmaps")]
    pub heightmaps: Heightmaps,

    #[nbt(rename = "Status")]
    pub status: &'de Cesu8,

    pub block_entities: Vec<BlockEntity<'de>>,
    // TODO:
    // pub block_ticks: Vec<Any>,
    // pub fluid_ticks: Vec<Any>,
}

impl ChunkPayload<'_> {
    pub fn normalize_hash(&self) -> Result<DiffHash, nanonbt::Error> {
        let normalized = NormalizedChunkPayload {
            data_version: self.data_version,
            x_pos: self.x_pos,
            y_pos: self.y_pos,
            z_pos: self.z_pos,
            sections: &self.sections,
            heightmaps: &self.heightmaps,
            status: self.status,
            block_entities: &self.block_entities,
        };
        let bytes = nanonbt::to_bytes(&normalized)?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(&bytes);
        Ok(DiffHash(*hasher.finalize().as_bytes()))
    }
}

#[derive(SerializeNbt, Debug)]
struct NormalizedChunkPayload<'a, 'de> {
    #[nbt(rename = "DataVersion")]
    pub data_version: i32,

    #[nbt(rename = "xPos")]
    pub x_pos: i32,
    #[nbt(rename = "yPos")]
    pub y_pos: i32,
    #[nbt(rename = "zPos")]
    pub z_pos: i32,

    pub sections: &'a [Section<'de>],

    #[nbt(rename = "Heightmaps")]
    pub heightmaps: &'a Heightmaps,

    #[nbt(rename = "Status")]
    pub status: &'a Cesu8,

    pub block_entities: &'a [BlockEntity<'de>],
    // TODO:
    // pub block_ticks: Vec<Any>,
    // pub fluid_ticks: Vec<Any>,
}
