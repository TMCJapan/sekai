use alloc::vec::Vec;
use nanonbt::{Cesu8, DeserializeNbt, I64Be, SerializeNbt, Tag};

#[derive(SerializeNbt, DeserializeNbt, Debug, Clone, Eq, PartialEq, Default)]
pub struct BlockStates<'de> {
    pub palette: Vec<BlockState<'de>>,
    #[nbt(array = long)]
    pub data: &'de [I64Be],
}

#[derive(SerializeNbt, DeserializeNbt, Debug, Clone, Eq, PartialEq)]
pub struct BlockState<'de> {
    #[nbt(rename = "Name")]
    pub name: &'de Cesu8,
    #[nbt(rename = "Properties")]
    pub properties: BlockProperties<'de>,
}

/// Raw remainder of a block state compound after `Name`.
///
/// Holds the verbatim `Properties` body; serialization writes it back
/// unchanged, so it hashes identically to the captured bytes.
#[derive(DeserializeNbt, Debug, Clone, Eq, PartialEq)]
pub struct BlockProperties<'de> {
    #[nbt(self)]
    pub payload: &'de [u8],
}

impl SerializeNbt for BlockProperties<'_> {
    const TAG: Tag = Tag::Compound;
    fn serialize_nbt<W: nanonbt::Write + ?Sized>(&self, writer: &mut W) -> nanonbt::Result<()> {
        writer.write_bytes(self.payload);
        Ok(())
    }
}
