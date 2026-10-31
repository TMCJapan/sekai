mod block_entity;
mod decode;
mod heightmap;
pub mod legacy;
mod section;
mod tick;

pub use block_entity::BlockEntity;
pub use heightmap::Heightmaps;
pub use legacy::{LegacyChunkPayload, LegacySection, RawCompound};
pub use section::Section;
pub use tick::{BlockTick, FluidTick};

use alloc::vec::Vec;
use nanonbt::{Cesu8, DeserializeNbt, SerializeNbt};
use sekai_util::DiffHash;

/// A parsed level chunk in either on-disk format.
///
/// Pre-1.13 worlds wrap their fields in `Level` and store sections as byte
/// arrays. Each format hashes through its own normalized view; the legacy
/// view writes keys the modern one never does, so a conversion is never
/// mistaken for "no change".
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Chunk<'de> {
    /// 1.13+ shape: fields at the root, palette sections.
    Modern(ChunkPayload<'de>),
    /// Pre-1.13 `Level` shape: byte-array sections.
    Legacy(LegacyChunkPayload<'de>),
}

impl Chunk<'_> {
    /// Hashes the chunk through the normalized view of its format.
    pub fn normalize_hash(&self) -> Result<DiffHash, nanonbt::Error> {
        match self {
            Self::Modern(payload) => payload.normalize_hash(),
            Self::Legacy(payload) => payload.normalize_hash(),
        }
    }
}

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
