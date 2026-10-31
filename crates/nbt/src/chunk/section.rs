mod biome;
mod block_state;

pub use biome::Biomes;
pub use block_state::BlockStates;

use nanonbt::{DeserializeNbtRef, ReadRef, SerializeNbt, Tag, read_name_into, skip_nbt};

use super::decode::read_byte_array;

#[derive(SerializeNbt, Debug, Clone, Eq, PartialEq, Default)]
pub struct Section<'de> {
    #[nbt(rename = "Y")]
    pub y: i8,
    pub block_states: BlockStates<'de>,
    pub biomes: Biomes<'de>,

    #[nbt(rename = "BlockLight", array = byte)]
    pub block_lights: &'de [u8],
    #[nbt(rename = "SkyLight", array = byte)]
    pub sky_lights: &'de [u8],
}

impl<'de> DeserializeNbtRef<'de> for Section<'de> {
    const TAG: Tag = Tag::Compound;

    fn deserialize_nbt_ref<R: ReadRef<'de> + ?Sized>(reader: &mut R) -> nanonbt::Result<Self> {
        let mut section = Self::default();
        // Longest known key is `block_states`; longer names are skipped.
        let mut name = [0u8; 12];

        loop {
            let tag = Tag::from_u8(reader.read_u8()?)?;
            if tag == Tag::End {
                break;
            }
            let Some(len) = read_name_into(reader, &mut name)? else {
                skip_nbt(reader, tag)?;
                continue;
            };

            match (&name[..len], tag) {
                (b"Y", Tag::Byte) => section.y = reader.read_u8()?.cast_signed(),
                (b"block_states", Tag::Compound) => {
                    section.block_states = BlockStates::deserialize_nbt_ref(reader)?;
                }
                (b"biomes", Tag::Compound) => {
                    section.biomes = Biomes::deserialize_nbt_ref(reader)?;
                }
                (b"BlockLight", Tag::ByteArray) => {
                    section.block_lights = read_byte_array(reader)?;
                }
                (b"SkyLight", Tag::ByteArray) => {
                    section.sky_lights = read_byte_array(reader)?;
                }
                _ => skip_nbt(reader, tag)?,
            }
        }

        Ok(section)
    }
}
