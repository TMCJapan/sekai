use alloc::vec::Vec;

use nanonbt::{
    Cesu8, DeserializeNbtRef, I64Be, ReadRef, SerializeNbt, Tag, read_name_into, skip_nbt,
};

use crate::chunk::decode::read_long_array;

#[derive(SerializeNbt, Debug, Clone, Eq, PartialEq, Default)]
pub struct Biomes<'de> {
    pub palette: Vec<&'de Cesu8>,
    #[nbt(array = long)]
    pub data: &'de [I64Be],
}

impl<'de> DeserializeNbtRef<'de> for Biomes<'de> {
    const TAG: Tag = Tag::Compound;

    fn deserialize_nbt_ref<R: ReadRef<'de> + ?Sized>(reader: &mut R) -> nanonbt::Result<Self> {
        let mut biomes = Self::default();
        let mut name = [0u8; 7];

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
                (b"palette", Tag::List) => {
                    biomes.palette = Vec::<&Cesu8>::deserialize_nbt_ref(reader)?;
                }
                (b"data", Tag::LongArray) => biomes.data = read_long_array(reader)?,
                _ => skip_nbt(reader, tag)?,
            }
        }

        Ok(biomes)
    }
}
