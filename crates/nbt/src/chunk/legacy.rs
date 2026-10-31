//! Pre-1.13 (`Level`-wrapped) chunk decoding.
//!
//! Legacy chunks keep their fields inside a `Level` compound, mark
//! generation progress with `TerrainPopulated` instead of `Status`, and
//! store block content as per-section `Blocks`/`Add`/`Data` byte arrays
//! rather than a palette. This module decodes the fields the diff needs;
//! the arrays are owned copies because the derive can only default a field
//! whose type owns its data, and pre-1.9 files omit most of them.

use alloc::vec::Vec;

use nanonbt::{DeserializeNbt, I32Be, SerializeNbt, Tag, Write};

/// A compound preserved verbatim as its raw payload bytes.
///
/// Legacy `TileEntities`/`TileTicks`/`LiquidTicks` entries carry fields this
/// crate never interprets, so they stay opaque: the bytes captured here are
/// exactly what the file held, including the terminating `TAG_End`, which is
/// what the normalized view hashes and what keeps a server rewrite of an
/// unrelated field visible.
#[derive(DeserializeNbt, Debug, Clone, Eq, PartialEq, Default)]
pub struct RawCompound<'de> {
    #[nbt(self)]
    raw: &'de [u8],
}

impl SerializeNbt for RawCompound<'_> {
    const TAG: Tag = Tag::Compound;

    fn serialize_nbt<W: Write + ?Sized>(&self, writer: &mut W) -> nanonbt::Result<()> {
        writer.write_bytes(self.raw);
        Ok(())
    }
}

/// One legacy section: the fields both formats share, plus the legacy block
/// arrays.
///
/// An `Anvil`-era section that already carried a palette has no `Blocks` and
/// decodes with empty arrays. `skip_serializing_if` keeps those empty arrays
/// out of the normalized view, so "absent" and "empty" hash alike.
#[derive(SerializeNbt, DeserializeNbt, Debug, Clone, Eq, PartialEq, Default)]
pub struct LegacySection<'de> {
    #[nbt(rename = "Y", default)]
    pub y: i8,

    #[nbt(
        rename = "BlockLight",
        array = byte,
        default,
        // TODO: skip_serializing_if = "<[u8]>::is_empty"
    )]
    pub block_lights: &'de [u8],
    #[nbt(
        rename = "SkyLight",
        array = byte,
        default,
        // TODO: skip_serializing_if = "<[u8]>::is_empty"
    )]
    pub sky_lights: &'de [u8],

    /// Low 8 bits of each block id, in `YZX` order (4096 bytes).
    #[nbt(
        rename = "Blocks",
        array = byte,
        default,
        // TODO: skip_serializing_if = "<[u8]>::is_empty"
    )]
    pub blocks: &'de [u8],
    /// High 4 bits of each block id, nibble-packed (2048 bytes).
    #[nbt(
        rename = "Add",
        array = byte,
        default,
        // TODO: skip_serializing_if = "<[u8]>::is_empty"
    )]
    pub add: &'de [u8],
    /// Block metadata, nibble-packed (2048 bytes).
    #[nbt(
        rename = "Data",
        array = byte,
        default,
        // TODO: skip_serializing_if = "<[u8]>::is_empty"
    )]
    pub data: &'de [u8],
    /// `Anvil` sections only: a second nibble array of unknown purpose,
    /// kept so untouched content hashes as it was captured.
    #[nbt(
        rename = "Data2",
        array = byte,
        default,
        // TODO: skip_serializing_if = "<[u8]>::is_empty"
    )]
    pub data2: &'de [u8],
}

/// A pre-1.13 chunk: the fields the diff view needs.
///
/// Sections and height maps keep their legacy shapes, and every field
/// defaults: pre-1.9 files omit most of them.
#[derive(DeserializeNbt, Debug, Clone, Eq, PartialEq, Default)]
pub struct LegacyChunkInner<'de> {
    /// `DataVersion` from the root (1.9+) or `Level`; `0` when absent.
    #[nbt(rename = "DataVersion", default)]
    pub data_version: i32,

    #[nbt(rename = "xPos", default)]
    pub x_pos: i32,
    #[nbt(rename = "yPos", default)]
    pub y_pos: i32,
    #[nbt(rename = "zPos", default)]
    pub z_pos: i32,

    #[nbt(rename = "InhabitedTime", default)]
    pub inhabited_time: i64,
    #[nbt(rename = "LastUpdate", default)]
    pub last_update: i64,

    #[nbt(rename = "TerrainPopulated", default)]
    pub terrain_populated: u8,

    #[nbt(rename = "Sections", default)]
    pub sections: Vec<LegacySection<'de>>,

    /// The 16-bit height map; empty when absent.
    #[nbt(rename = "HeightMap", array = int, default)]
    pub height_map: &'de [I32Be],

    #[nbt(rename = "TileEntities", default)]
    pub block_entities: Vec<RawCompound<'de>>,

    #[nbt(rename = "TileTicks", default)]
    pub block_ticks: Vec<RawCompound<'de>>,
    #[nbt(rename = "LiquidTicks", default)]
    pub fluid_ticks: Vec<RawCompound<'de>>,
}

impl LegacyChunkPayload<'_> {
    /// Hashes the payload through its normalized view.
    ///
    /// Volatile tags (`InhabitedTime`, `LastUpdate`) are excluded as in the
    /// modern view. The view also carries keys the modern one never writes
    /// (`HeightMap`, the legacy section encoding), so legacy and modern
    /// content can never normalize equal: a format conversion is a real
    /// change, not a silent no-op.
    pub fn normalize_hash(&self) -> Result<sekai_util::DiffHash, nanonbt::Error> {
        let normalized = NormalizedLegacyChunkPayload {
            data_version: self.data_version,
            x_pos: self.level.x_pos,
            y_pos: self.level.y_pos,
            z_pos: self.level.z_pos,
            sections: &self.level.sections,
            height_map: self.level.height_map,
            terrain_populated: self.level.terrain_populated,
            block_entities: &self.level.block_entities,
            block_ticks: &self.level.block_ticks,
            fluid_ticks: &self.level.fluid_ticks,
        };
        let bytes = nanonbt::to_bytes(&normalized)?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(&bytes);
        Ok(sekai_util::DiffHash(*hasher.finalize().as_bytes()))
    }
}

/// The hash input of [`LegacyChunkPayload::normalize_hash`].
///
/// The same rule as [`super::NormalizedChunkPayload`]: volatile tags stay
/// out, and a new payload field joins change detection only once it is added
/// here as well. Empty collections are omitted, so absent and empty data
/// normalize alike.
#[derive(SerializeNbt, Debug)]
struct NormalizedLegacyChunkPayload<'a, 'de> {
    data_version: i32,

    x_pos: i32,
    y_pos: i32,
    z_pos: i32,

    #[nbt(skip_serializing_if = "slice_is_empty")]
    sections: &'a [LegacySection<'de>],

    #[nbt(
        array = int,
        skip_serializing_if = "slice_is_empty"
    )]
    height_map: &'a [I32Be],

    terrain_populated: u8,

    #[nbt(skip_serializing_if = "slice_is_empty")]
    block_entities: &'a [RawCompound<'de>],
    #[nbt(skip_serializing_if = "slice_is_empty")]
    block_ticks: &'a [RawCompound<'de>],
    #[nbt(skip_serializing_if = "slice_is_empty")]
    fluid_ticks: &'a [RawCompound<'de>],
}

/// Predicate for `skip_serializing_if` on slice fields.
const fn slice_is_empty<T>(value: &&[T]) -> bool {
    value.is_empty()
}

/// The root of a legacy document: `Level` plus the root-level `DataVersion`
/// that 1.9+ writes next to it.
#[derive(DeserializeNbt, Debug, Default, Clone, Eq, PartialEq)]
pub struct LegacyChunkPayload<'de> {
    #[nbt(rename = "Level")]
    level: LegacyChunkInner<'de>,

    #[nbt(rename = "DataVersion", default)]
    data_version: i32,
}

#[cfg(test)]
mod tests;
