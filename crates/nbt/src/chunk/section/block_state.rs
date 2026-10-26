use alloc::vec::Vec;
use nanonbt::{Cesu8, DeserializeNbt, I64Be, SerializeNbt};

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
    // TODO:
    // #[nbt(rename = "Properties")]
    // properties: Any,
}
