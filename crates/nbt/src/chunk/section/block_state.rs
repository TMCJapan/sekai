use alloc::vec::Vec;

use nanonbt::{
    Cesu8, DeserializeNbt, DeserializeNbtRef, I64Be, ReadRef, SerializeNbt, Tag, read_name_into,
    skip_nbt,
};

use crate::chunk::decode::read_long_array;

#[derive(SerializeNbt, Debug, Clone, Eq, PartialEq, Default)]
pub struct BlockStates<'de> {
    pub palette: Vec<BlockState<'de>>,
    #[nbt(array = long)]
    pub data: &'de [I64Be],
}

impl<'de> DeserializeNbtRef<'de> for BlockStates<'de> {
    const TAG: Tag = Tag::Compound;

    fn deserialize_nbt_ref<R: ReadRef<'de> + ?Sized>(reader: &mut R) -> nanonbt::Result<Self> {
        let mut states = Self::default();
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
                    states.palette = Vec::<BlockState>::deserialize_nbt_ref(reader)?;
                }
                (b"data", Tag::LongArray) => states.data = read_long_array(reader)?,
                _ => skip_nbt(reader, tag)?,
            }
        }

        Ok(states)
    }
}

#[derive(SerializeNbt, Debug, Clone, Eq, PartialEq)]
pub struct BlockState<'de> {
    #[nbt(rename = "Name")]
    pub name: &'de Cesu8,
    #[nbt(rename = "Properties")]
    pub properties: BlockProperties<'de>,
}

impl<'de> DeserializeNbtRef<'de> for BlockState<'de> {
    const TAG: Tag = Tag::Compound;

    fn deserialize_nbt_ref<R: ReadRef<'de> + ?Sized>(reader: &mut R) -> nanonbt::Result<Self> {
        let mut name = None;
        let mut properties = BlockProperties::default();
        let mut field = [0u8; 10];

        loop {
            let tag = Tag::from_u8(reader.read_u8()?)?;
            if tag == Tag::End {
                break;
            }
            let Some(len) = read_name_into(reader, &mut field)? else {
                skip_nbt(reader, tag)?;
                continue;
            };

            match (&field[..len], tag) {
                (b"Name", Tag::String) => {
                    name = Some(<&Cesu8>::deserialize_nbt_ref(reader)?);
                }
                (b"Properties", Tag::Compound) => {
                    properties = BlockProperties::deserialize_nbt_ref(reader)?;
                }
                _ => skip_nbt(reader, tag)?,
            }
        }

        Ok(Self {
            name: name.ok_or(nanonbt::Error::MissingField("Name"))?,
            properties,
        })
    }
}

/// Raw body of a block state's `Properties` compound.
///
/// A missing `Properties` key decodes as an empty compound body (`TAG_End`).
/// Serialization writes the body back unchanged, including the trailing
/// `TAG_End`.
#[derive(DeserializeNbt, Debug, Clone, Eq, PartialEq)]
pub struct BlockProperties<'de> {
    #[nbt(self)]
    pub payload: &'de [u8],
}

const EMPTY_COMPOUND: &[u8] = &[Tag::End.as_u8()];

impl Default for BlockProperties<'_> {
    fn default() -> Self {
        Self {
            payload: EMPTY_COMPOUND,
        }
    }
}

impl SerializeNbt for BlockProperties<'_> {
    const TAG: Tag = Tag::Compound;

    fn serialize_nbt<W: nanonbt::Write + ?Sized>(&self, writer: &mut W) -> nanonbt::Result<()> {
        if self.payload.is_empty() {
            writer.write_u8(Tag::End.as_u8());
        } else {
            writer.write_bytes(self.payload);
        }
        Ok(())
    }
}
