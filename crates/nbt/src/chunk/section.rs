mod biome;
mod block_state;

pub use biome::Biomes;
pub use block_state::BlockStates;

use alloc::vec::Vec;
use nanonbt::{DeserializeNbt, Read, SerializeNbt, Tag};

#[derive(SerializeNbt, Debug, Clone, Eq, PartialEq, Default)]
pub struct Section {
    #[nbt(rename = "Y")]
    pub y: i8,
    pub block_states: BlockStates,
    pub biomes: Biomes,

    #[nbt(rename = "BlockLights", array = byte)]
    pub block_lights: Vec<u8>,
    #[nbt(rename = "SkyLights", array = byte)]
    pub sky_lights: Vec<u8>,
}

impl DeserializeNbt for Section {
    const TAG: Tag = Tag::Compound;

    fn deserialize_nbt<R: Read + ?Sized>(r: &mut R) -> Result<Self, nanonbt::Error> {
        let mut y: Option<i8> = None;
        let mut block_states = BlockStates::default();
        let mut biomes = Biomes::default();
        let mut block_lights: Vec<u8> = Vec::new();
        let mut sky_lights: Vec<u8> = Vec::new();

        loop {
            let tag = Tag::from_u8(r.read_u8()?)?;
            if tag == Tag::End {
                break;
            }
            let name_len = r.read_u16_be()? as usize;
            let mut name_buf = alloc::vec![0u8; name_len];
            r.read_exact(&mut name_buf)?;
            let name = core::str::from_utf8(&name_buf).unwrap_or("");

            match (name, tag) {
                ("Y", Tag::Byte) => {
                    y = Some(r.read_u8()?.cast_signed());
                }
                ("block_states", Tag::Compound) => {
                    block_states = BlockStates::deserialize_nbt(r)?;
                }
                ("biomes", Tag::Compound) => {
                    biomes = Biomes::deserialize_nbt(r)?;
                }
                ("BlockLights", Tag::ByteArray) => {
                    let n = usize::try_from(r.read_i32_be()?)
                        .map_err(|_| nanonbt::Error::InvalidLength)?;
                    let mut buf = alloc::vec![0u8; n];
                    r.read_exact(&mut buf)?;
                    block_lights = buf;
                }
                ("SkyLights", Tag::ByteArray) => {
                    let n = usize::try_from(r.read_i32_be()?)
                        .map_err(|_| nanonbt::Error::InvalidLength)?;
                    let mut buf = alloc::vec![0u8; n];
                    r.read_exact(&mut buf)?;
                    sky_lights = buf;
                }
                _ => nanonbt::skip_nbt(r, tag)?,
            }
        }

        Ok(Self {
            y: y.unwrap_or(0),
            block_states,
            biomes,
            block_lights,
            sky_lights,
        })
    }
}
