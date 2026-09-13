//! Chunk coordinates and namespace identifiers.
//!
//! Rationale: `Dimension`/`RegionKind` are transparent integer wrappers
//! rather than stringly-typed names or closed enums. Integers keep the
//! SQLite schema compact (no string interning table), stay `Copy` for
//! hot-path iteration, and remain forward compatible with modded
//! dimensions or future region kinds without breaking stored history.

/// Namespace for a dimension (e.g. overworld/nether/end or a modded one).
///
/// Well-known values follow the vanilla numeric convention so that the
/// mapping stays obvious; custom dimensions use any other value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Dimension(pub i32);

impl Dimension {
    /// Vanilla overworld.
    pub const OVERWORLD: Self = Self(0);
    /// Vanilla nether.
    pub const NETHER: Self = Self(-1);
    /// Vanilla end.
    pub const END: Self = Self(1);

    /// Raw numeric identifier.
    #[inline]
    #[must_use]
    pub const fn raw(self) -> i32 {
        self.0
    }
}

/// Which `.mca` family a chunk belongs to (`region/`, `entities/`, `poi/`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegionKind(pub u8);

impl RegionKind {
    /// Block data (`region/r.<x>.<z>.mca`).
    pub const REGION: Self = Self(0);
    /// Entity data (`entities/r.<x>.<z>.mca`, 1.17+ split).
    pub const ENTITIES: Self = Self(1);
    /// Point-of-interest data (`poi/r.<x>.<z>.mca`).
    pub const POI: Self = Self(2);

    /// Raw numeric identifier.
    #[inline]
    #[must_use]
    pub const fn raw(self) -> u8 {
        self.0
    }
}

/// Global identity of one chunk column across all snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChunkCoord {
    /// Dimension namespace.
    pub dim: Dimension,
    /// Region file family.
    pub kind: RegionKind,
    /// Global chunk X.
    pub x: i32,
    /// Global chunk Z.
    pub z: i32,
}

impl ChunkCoord {
    /// Construct a coordinate. All `i32` positions are valid by construction.
    #[inline]
    #[must_use]
    pub const fn new(dim: Dimension, kind: RegionKind, x: i32, z: i32) -> Self {
        Self { dim, kind, x, z }
    }

    /// Region file X (`floor(x / 32)` via arithmetic shift, negative-safe).
    #[inline]
    #[must_use]
    pub const fn region_x(self) -> i32 {
        self.x >> 5
    }

    /// Region file Z (`floor(z / 32)` via arithmetic shift, negative-safe).
    #[inline]
    #[must_use]
    pub const fn region_z(self) -> i32 {
        self.z >> 5
    }

    /// Local X inside the region file (`x mod 32`, always `0..32`).
    #[inline]
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub const fn local_x(self) -> u8 {
        self.x.rem_euclid(32) as u8
    }

    /// Local Z inside the region file (`z mod 32`, always `0..32`).
    #[inline]
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub const fn local_z(self) -> u8 {
        self.z.rem_euclid(32) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_coords_map_to_correct_region() {
        // Arithmetic shift keeps floor semantics; truncation would not.
        let c = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::REGION, -1, -33);
        assert_eq!(c.region_x(), -1);
        assert_eq!(c.region_z(), -2);
        assert_eq!(c.local_x(), 31);
        assert_eq!(c.local_z(), 31);
    }

    #[test]
    fn positive_coords_map_to_correct_region() {
        let c = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::REGION, 32, 33);
        assert_eq!(c.region_x(), 1);
        assert_eq!(c.region_z(), 1);
        assert_eq!(c.local_x(), 0);
        assert_eq!(c.local_z(), 1);
    }
}
