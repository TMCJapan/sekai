//! Chunk coordinates and namespace identifiers.

/// Dimension namespace code. `0..=2` are reserved for vanilla dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Dimension(i32);

impl Dimension {
    /// Vanilla overworld.
    pub const OVERWORLD: Self = Self(0);
    /// Vanilla nether.
    pub const NETHER: Self = Self(1);
    /// Vanilla end.
    pub const END: Self = Self(2);

    /// Wrap a raw code.
    pub const fn new(raw: i32) -> Self {
        Self(raw)
    }

    /// Raw code (for binary columns and file names).
    pub const fn raw(self) -> i32 {
        self.0
    }
}

/// Region family identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegionKind(i32);

impl RegionKind {
    /// Block data (`region/`).
    pub const REGION: Self = Self(0);
    /// Entity data (`entities/`).
    pub const ENTITIES: Self = Self(1);
    /// Points of interest (`poi/`).
    pub const POI: Self = Self(2);

    /// Wrap a raw code.
    pub const fn new(raw: i32) -> Self {
        Self(raw)
    }

    /// Raw code (for binary columns and directory names).
    pub const fn raw(self) -> i32 {
        self.0
    }
}

/// Global chunk identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChunkCoord {
    /// Dimension namespace.
    pub dim: Dimension,
    /// Region family.
    pub kind: RegionKind,
    /// Global chunk X.
    pub x: i32,
    /// Global chunk Z.
    pub z: i32,
}

impl ChunkCoord {
    /// Construct a chunk identity.
    pub const fn new(dim: Dimension, kind: RegionKind, x: i32, z: i32) -> Self {
        Self { dim, kind, x, z }
    }

    /// Owning region X (`x.div_euclid(32)`, correct for negatives).
    pub const fn region_x(self) -> i32 {
        self.x.div_euclid(32)
    }

    /// Owning region Z.
    pub const fn region_z(self) -> i32 {
        self.z.div_euclid(32)
    }
}

/// Resolve a custom dimension path to a stable non-vanilla identifier.
pub fn resolve_custom_dimension(relative: &str) -> Dimension {
    let raw = sekai_anvil::custom_dimension_id(relative);
    Dimension(remap_reserved(raw))
}

/// Keep custom identifiers out of the reserved vanilla range.
const fn remap_reserved(raw: i32) -> i32 {
    if raw == Dimension::OVERWORLD.0 || raw == Dimension::NETHER.0 || raw == Dimension::END.0 {
        raw.wrapping_add(0x0100_0000)
    } else {
        raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_of_negative_coords() {
        let c = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::REGION, -1, -33);
        assert_eq!((c.region_x(), c.region_z()), (-1, -2));
        let c = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::REGION, 0, 31);
        assert_eq!((c.region_x(), c.region_z()), (0, 0));
        let c = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::REGION, 32, 32);
        assert_eq!((c.region_x(), c.region_z()), (1, 1));
    }

    #[test]
    fn reserved_codes_remap_away() {
        assert_eq!(remap_reserved(0), 0x0100_0000);
        assert_eq!(remap_reserved(1), 0x0100_0001);
        assert_eq!(remap_reserved(2), 0x0100_0002);
        assert_eq!(remap_reserved(3), 3);
        assert_eq!(remap_reserved(-5), -5);
    }

    #[test]
    fn custom_dimensions_are_stable_and_vanilla_free() {
        let a = resolve_custom_dimension("dimensions/aether/sky");
        assert_eq!(a, resolve_custom_dimension("dimensions/aether/sky"));
        assert_ne!(a, resolve_custom_dimension("dimensions/aether/other"));
        assert_ne!(a, Dimension::OVERWORLD);
        assert_ne!(a, Dimension::NETHER);
        assert_ne!(a, Dimension::END);
    }
}
