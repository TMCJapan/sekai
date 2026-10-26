mod biome;
mod block_state;

pub use biome::Biomes;
pub use block_state::BlockStates;

use nanonbt::{DeserializeNbt, SerializeNbt};

#[derive(SerializeNbt, DeserializeNbt, Debug, Clone, Eq, PartialEq, Default)]
pub struct Section<'de> {
    #[nbt(rename = "Y")]
    pub y: i8,
    pub block_states: BlockStates<'de>,
    pub biomes: Biomes<'de>,

    #[nbt(rename = "BlockLights", array = byte)]
    pub block_lights: &'de [u8],
    #[nbt(rename = "SkyLights", array = byte)]
    pub sky_lights: &'de [u8],
}
