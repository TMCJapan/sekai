use nanonbt::{Cesu8, DeserializeNbt, SerializeNbt};

#[derive(SerializeNbt, DeserializeNbt, Debug, Clone, Eq, PartialEq)]
pub struct ScheduledTick<'de> {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    #[nbt(rename = "t")]
    pub tick: i32,
    #[nbt(rename = "p")]
    pub priority: i32,
    #[nbt(rename = "i")]
    pub id: &'de Cesu8,
}

pub type BlockTick<'de> = ScheduledTick<'de>;
pub type FluidTick<'de> = ScheduledTick<'de>;
