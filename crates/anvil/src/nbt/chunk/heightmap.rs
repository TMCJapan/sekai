use nanonbt::{DeserializeNbtRef, I64Be, ReadRef, SerializeNbt, Tag, read_name_into, skip_nbt};

use super::decode::read_long_array;

#[derive(SerializeNbt, Debug, Clone, Eq, PartialEq, Default)]
pub struct Heightmaps<'de> {
    #[nbt(rename = "MOTION_BLOCKING", array = long)]
    pub motion_blocking: &'de [I64Be],

    #[nbt(rename = "MOTION_BLOCKING_NO_LEAVES", array = long)]
    pub motion_blocking_no_leaves: &'de [I64Be],

    #[nbt(rename = "OCEAN_FLOOR", array = long)]
    pub ocean_floor: &'de [I64Be],

    #[nbt(rename = "OCEAN_FLOOR_WG", array = long)]
    pub ocean_floor_wg: &'de [I64Be],

    #[nbt(rename = "WORLD_SURFACE", array = long)]
    pub world_surface: &'de [I64Be],

    #[nbt(rename = "WORLD_SURFACE_WG", array = long)]
    pub world_surface_wg: &'de [I64Be],
}

impl<'de> DeserializeNbtRef<'de> for Heightmaps<'de> {
    const TAG: Tag = Tag::Compound;

    fn deserialize_nbt_ref<R: ReadRef<'de> + ?Sized>(reader: &mut R) -> nanonbt::Result<Self> {
        let mut heightmaps = Self::default();
        // Longest known key is `MOTION_BLOCKING_NO_LEAVES`.
        let mut name = [0u8; 25];

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
                (b"MOTION_BLOCKING", Tag::LongArray) => {
                    heightmaps.motion_blocking = read_long_array(reader)?;
                }
                (b"MOTION_BLOCKING_NO_LEAVES", Tag::LongArray) => {
                    heightmaps.motion_blocking_no_leaves = read_long_array(reader)?;
                }
                (b"OCEAN_FLOOR", Tag::LongArray) => {
                    heightmaps.ocean_floor = read_long_array(reader)?;
                }
                (b"OCEAN_FLOOR_WG", Tag::LongArray) => {
                    heightmaps.ocean_floor_wg = read_long_array(reader)?;
                }
                (b"WORLD_SURFACE", Tag::LongArray) => {
                    heightmaps.world_surface = read_long_array(reader)?;
                }
                (b"WORLD_SURFACE_WG", Tag::LongArray) => {
                    heightmaps.world_surface_wg = read_long_array(reader)?;
                }
                _ => skip_nbt(reader, tag)?,
            }
        }

        Ok(heightmaps)
    }
}
