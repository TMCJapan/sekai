use alloc::vec::Vec;
use nanonbt::{Cesu8, DeserializeNbt, I64Be, SerializeNbt};

#[derive(SerializeNbt, DeserializeNbt, Debug, Clone, Eq, PartialEq, Default)]
pub struct Biomes<'de> {
    pub palette: Vec<&'de Cesu8>,
    #[nbt(array = long)]
    pub data: &'de [I64Be],
}
