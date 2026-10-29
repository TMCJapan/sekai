mod block_entity;
mod decode;
mod heightmap;
mod section;
mod tick;

pub use block_entity::BlockEntity;
pub use heightmap::Heightmaps;
pub use section::Section;
pub use tick::{BlockTick, FluidTick};

use alloc::vec::Vec;
use nanonbt::{Cesu8, DeserializeNbt, SerializeNbt};
use sekai_util::DiffHash;

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
    pub heightmaps: Heightmaps<'de>,

    #[nbt(rename = "Status")]
    pub status: &'de Cesu8,

    pub block_entities: Vec<BlockEntity<'de>>,

    pub block_ticks: Vec<BlockTick<'de>>,
    pub fluid_ticks: Vec<FluidTick<'de>>,
}

impl ChunkPayload<'_> {
    /// Hashes the payload through its normalized view.
    ///
    /// Everything except the volatile `InhabitedTime`/`LastUpdate` tags is
    /// encoded; the view is defined next to this impl.
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
            block_ticks: &self.block_ticks,
            fluid_ticks: &self.fluid_ticks,
        };
        let bytes = nanonbt::to_bytes(&normalized)?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(&bytes);
        Ok(DiffHash(*hasher.finalize().as_bytes()))
    }
}

/// The hash input of [`ChunkPayload::normalize_hash`].
///
/// `InhabitedTime` and `LastUpdate` are deliberately absent: the server
/// rewrites them on its own, so they must not register as changes. New
/// payload fields participate in change detection only once they are added
/// here as well.
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
    pub heightmaps: &'a Heightmaps<'de>,

    #[nbt(rename = "Status")]
    pub status: &'a Cesu8,

    pub block_entities: &'a [BlockEntity<'de>],

    pub block_ticks: &'a [BlockTick<'de>],
    pub fluid_ticks: &'a [FluidTick<'de>],
}
