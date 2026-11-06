use nanonbt::{DeserializeNbt, SerializeNbt, Tag, Write};

/// A compound preserved verbatim as its raw payload bytes.
///
/// Used for compounds whose fields this crate never interprets, such as
/// legacy `TileEntities` entries or the modern `structures` tree. The bytes
/// captured here are exactly what the file held, including the terminating
/// `TAG_End`, which is what the normalized views hash and what keeps a
/// server rewrite of an unrelated field visible.
///
/// An absent key decodes as the default, which holds no bytes at all; a
/// present compound always carries at least its `TAG_End`, so
/// [`Self::is_absent`] tells the two apart.
#[derive(DeserializeNbt, Debug, Clone, Copy, Eq, PartialEq, Default)]
pub struct RawCompound<'de> {
    #[nbt(self)]
    raw: &'de [u8],
}

impl RawCompound<'_> {
    /// Whether this value stands in for a missing key rather than a decoded
    /// compound.
    pub const fn is_absent(&self) -> bool {
        self.raw.is_empty()
    }
}

impl SerializeNbt for RawCompound<'_> {
    const TAG: Tag = Tag::Compound;

    fn serialize_nbt<W: Write + ?Sized>(&self, writer: &mut W) -> nanonbt::Result<()> {
        writer.write_bytes(self.raw);
        Ok(())
    }
}
