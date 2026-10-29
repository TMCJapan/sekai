use nanonbt::{Cesu8, DeserializeNbt, SerializeNbt, Tag};

#[derive(DeserializeNbt, Debug, Clone, Eq, PartialEq)]
pub struct BlockEntity<'de> {
    pub id: &'de Cesu8,
    pub x: i32,
    pub y: i32,
    pub z: i32,

    /// Raw remainder of the compound captured after the typed header.
    ///
    /// Holds the verbatim body minus `id`/`x`/`y`/`z`; serialization writes
    /// it back unchanged, so it hashes identically to the captured bytes.
    #[nbt(self)]
    pub payload: &'de [u8],
}

impl SerializeNbt for BlockEntity<'_> {
    const TAG: Tag = Tag::Compound;
    fn serialize_nbt<W: nanonbt::Write + ?Sized>(&self, writer: &mut W) -> nanonbt::Result<()> {
        writer.write_bytes(self.payload);
        Ok(())
    }
}
