use nanonbt::route::{ByteArray, LongArray};
use nanonbt::{DeserializeNbtArrayRef, I64Be, ReadRef};

/// Reads a `TAG_Byte_Array` payload.
pub(super) fn read_byte_array<'de, R: ReadRef<'de> + ?Sized>(
    reader: &mut R,
) -> nanonbt::Result<&'de [u8]> {
    <&'de [u8] as DeserializeNbtArrayRef<'de, ByteArray>>::deserialize_nbt_array_ref(reader)
}

/// Reads a `TAG_Long_Array` payload.
pub(super) fn read_long_array<'de, R: ReadRef<'de> + ?Sized>(
    reader: &mut R,
) -> nanonbt::Result<&'de [I64Be]> {
    <&'de [I64Be] as DeserializeNbtArrayRef<'de, LongArray>>::deserialize_nbt_array_ref(reader)
}
