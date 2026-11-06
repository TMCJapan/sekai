mod block_entity;
mod decode;
mod heightmap;
pub mod legacy;
mod raw_compound;
mod section;
mod tick;

pub use block_entity::BlockEntity;
pub use heightmap::Heightmaps;
pub use legacy::LegacyChunkPayload;
pub use raw_compound::RawCompound;
pub use section::Section;
use sekai_util::ChunkHash;
pub use tick::{BlockTick, FluidTick};

use alloc::vec::Vec;
use nanonbt::{Cesu8, DeserializeNbt, SerializeNbt};

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
    pub fn normalized_hash(&self) -> Result<sekai_util::ChunkHash, nanonbt::Error> {
        match self {
            Self::Modern(payload) => payload.normalized_hash(),
            Self::Legacy(payload) => payload.normalized_hash(),
        }
    }
}

/// A 1.18+ level chunk, fields in the order of the Minecraft Wiki's
/// [Chunk format](https://minecraft.wiki/w/Chunk_format) root listing.
///
/// Keys not declared here are skipped on decode and so never reach the
/// chunk key. The fields added after the original schema default when
/// absent, so a chunk that predates or omits them still parses.
#[derive(SerializeNbt, DeserializeNbt, Debug, Clone, Eq, PartialEq)]
pub struct ChunkPayload<'de> {
    #[nbt(rename = "DataVersion")]
    pub data_version: i32,

    #[nbt(rename = "xPos")]
    pub x_pos: i32,
    #[nbt(rename = "zPos")]
    pub z_pos: i32,
    #[nbt(rename = "yPos")]
    pub y_pos: i32,

    /// Renamed to `status` in 26.4. Both spellings decode here and the
    /// normalized view writes `Status`, so the rename alone is no change.
    #[nbt(rename = "Status", alias = "status")]
    pub status: &'de Cesu8,

    #[nbt(rename = "LastUpdate")]
    pub last_update: i64,

    pub sections: Vec<Section<'de>>,

    pub block_entities: Vec<BlockEntity<'de>>,

    #[nbt(rename = "Heightmaps")]
    pub heightmaps: Heightmaps<'de>,

    pub fluid_ticks: Vec<FluidTick<'de>>,
    pub block_ticks: Vec<BlockTick<'de>>,

    #[nbt(rename = "InhabitedTime")]
    pub inhabited_time: i64,

    /// Present only next to pre-1.18 terrain. Opaque because its heights
    /// are `Double`s, which cannot satisfy `Eq`.
    #[nbt(default, skip_serializing_if = "RawCompound::is_absent")]
    pub blending_data: RawCompound<'de>,

    /// Packed in-section positions of blocks awaiting a post-generation
    /// update, one list per section.
    #[nbt(rename = "PostProcessing", default)]
    pub post_processing: Vec<Vec<i16>>,

    /// Structure starts and references, kept opaque: its layout varies per
    /// structure type.
    #[nbt(default, skip_serializing_if = "RawCompound::is_absent")]
    pub structures: RawCompound<'de>,

    /// Not on the Wiki listing, but written by vanilla into every chunk.
    #[nbt(rename = "isLightOn", default)]
    pub is_light_on: bool,
}

#[cfg(test)]
impl ChunkPayload<'static> {
    /// A fully generated chunk at the origin with every list empty and every
    /// optional key absent, for tests to override with struct update syntax.
    pub(crate) fn empty_full() -> Self {
        Self {
            data_version: 1,
            x_pos: 0,
            z_pos: 0,
            y_pos: 0,
            status: Cesu8::try_from_str("minecraft:full").unwrap(),
            last_update: 0,
            sections: Vec::new(),
            block_entities: Vec::new(),
            heightmaps: Heightmaps::default(),
            fluid_ticks: Vec::new(),
            block_ticks: Vec::new(),
            inhabited_time: 0,
            blending_data: RawCompound::default(),
            post_processing: Vec::new(),
            structures: RawCompound::default(),
            is_light_on: false,
        }
    }
}

impl ChunkPayload<'_> {
    /// Hashes the payload through its normalized view.
    ///
    /// Everything except the volatile `InhabitedTime`/`LastUpdate` tags is
    /// encoded; the view is defined next to this impl.
    pub fn normalized_hash(&self) -> Result<sekai_util::ChunkHash, nanonbt::Error> {
        let normalized = NormalizedChunkPayload {
            data_version: self.data_version,
            x_pos: self.x_pos,
            z_pos: self.z_pos,
            y_pos: self.y_pos,
            status: self.status,
            sections: &self.sections,
            block_entities: &self.block_entities,
            heightmaps: &self.heightmaps,
            fluid_ticks: &self.fluid_ticks,
            block_ticks: &self.block_ticks,
            blending_data: self.blending_data,
            post_processing: &self.post_processing,
            structures: self.structures,
            is_light_on: self.is_light_on,
        };
        let bytes = nanonbt::to_bytes(&normalized)?;
        Ok(ChunkHash::new(sekai_util::hash_bytes(&bytes)))
    }
}

/// The hash input of [`ChunkPayload::normalized_hash`].
///
/// `InhabitedTime` and `LastUpdate` are deliberately absent: the server
/// rewrites them on its own, so they must not register as changes. New
/// payload fields participate in the chunk key and change detection only
/// once they are added here as well.
///
/// Unlike the legacy view, empty lists and a `false` `isLightOn` are written
/// rather than omitted. A missing key decodes to that same default, so
/// absent and empty still normalize alike; only the opaque compounds keep
/// the two apart, as [`RawCompound`] describes.
#[derive(SerializeNbt, Debug)]
struct NormalizedChunkPayload<'a, 'de> {
    #[nbt(rename = "DataVersion")]
    pub data_version: i32,

    #[nbt(rename = "xPos")]
    pub x_pos: i32,
    #[nbt(rename = "zPos")]
    pub z_pos: i32,
    #[nbt(rename = "yPos")]
    pub y_pos: i32,

    #[nbt(rename = "Status")]
    pub status: &'a Cesu8,

    pub sections: &'a [Section<'de>],

    pub block_entities: &'a [BlockEntity<'de>],

    #[nbt(rename = "Heightmaps")]
    pub heightmaps: &'a Heightmaps<'de>,

    pub fluid_ticks: &'a [FluidTick<'de>],
    pub block_ticks: &'a [BlockTick<'de>],

    #[nbt(skip_serializing_if = "RawCompound::is_absent")]
    pub blending_data: RawCompound<'de>,

    #[nbt(rename = "PostProcessing")]
    pub post_processing: &'a [Vec<i16>],

    #[nbt(skip_serializing_if = "RawCompound::is_absent")]
    pub structures: RawCompound<'de>,

    #[nbt(rename = "isLightOn")]
    pub is_light_on: bool,
}
