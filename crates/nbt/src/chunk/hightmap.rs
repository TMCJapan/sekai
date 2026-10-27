use nanonbt::{DeserializeNbt, I64Be, SerializeNbt};

#[derive(SerializeNbt, DeserializeNbt, Debug, Clone, Eq, PartialEq, Default)]
pub struct Heightmaps<'de> {
    #[nbt(rename = "MOTION_BLOCKING", array = long)]
    pub motion_blocking: &'de [I64Be],

    #[nbt(rename = "MOTION_BLOCKING_NO_LEAVES", array = long)]
    pub motion_blocking_no_leaves: &'de [I64Be],

    #[nbt(rename = "OCEAN_FLOOR", array = long)]
    pub ocean_floor: &'de [I64Be],

    #[nbt(rename = "OCEAN_FLOOR_WG", array = long)]
    pub ocean_floor_wg: &'de [I64Be],

    #[nbt(rename = "WORLD_SURFACE", array = long)]
    pub world_surface: &'de [I64Be],

    #[nbt(rename = "WORLD_SURFACE_WG", array = long)]
    pub world_surface_wg: &'de [I64Be],
}
