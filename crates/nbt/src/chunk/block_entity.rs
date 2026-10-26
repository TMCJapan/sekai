use nanonbt::{Cesu8, DeserializeNbt, SerializeNbt};

#[derive(SerializeNbt, DeserializeNbt, Debug, Clone, Eq, PartialEq)]
pub struct BlockEntity<'de> {
    pub id: &'de Cesu8,
    pub x: i32,
    pub y: i32,
    pub z: i32,
    // TODO
}
