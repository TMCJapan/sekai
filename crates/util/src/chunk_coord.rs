use crate::dimension::Dimension;
use crate::region_kind::RegionKind;

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
}
