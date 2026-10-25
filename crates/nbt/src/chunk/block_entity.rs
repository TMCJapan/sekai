use alloc::string::String;
use nanonbt::{DeserializeNbt, SerializeNbt};

#[derive(SerializeNbt, DeserializeNbt, Debug, Clone, Eq, PartialEq)]
pub struct BlockEntity {
    pub id: String,
    pub x: i32,
    pub y: i32,
    pub z: i32,
    // TODO
}
