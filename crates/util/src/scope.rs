//! Backup/rollback/diff scope: whole world or per-dimension areas.
//!
//! Each selected dimension takes whole, rectangle, or chunk-list areas;
//! region kinds apply uniformly across all areas. The selection owns its
//! data so worker tasks can clone it across `'static` spawn boundaries.

use alloc::vec::Vec;

use super::{ChunkCoord, Dimension, RegionKey, RegionKind};

/// Inclusive chunk-coordinate rectangle, normalized so minima come first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Minimum chunk X.
    pub x0: i32,
    /// Minimum chunk Z.
    pub z0: i32,
    /// Maximum chunk X.
    pub x1: i32,
    /// Maximum chunk Z.
    pub z1: i32,
}

impl Rect {
    /// Build a rectangle from any two corners, normalizing order.
    pub const fn new(ax: i32, az: i32, bx: i32, bz: i32) -> Self {
        Self {
            x0: if ax <= bx { ax } else { bx },
            z0: if az <= bz { az } else { bz },
            x1: if ax <= bx { bx } else { ax },
            z1: if az <= bz { bz } else { az },
        }
    }

    /// Whole region file `(rx, rz)` as a rectangle.
    pub const fn region(rx: i32, rz: i32) -> Self {
        Self {
            x0: rx.saturating_mul(32),
            z0: rz.saturating_mul(32),
            x1: rx.saturating_mul(32).saturating_add(31),
            z1: rz.saturating_mul(32).saturating_add(31),
        }
    }

    /// Whether a chunk coordinate falls inside the rectangle.
    pub const fn contains(self, x: i32, z: i32) -> bool {
        self.x0 <= x && x <= self.x1 && self.z0 <= z && z <= self.z1
    }

    /// Whether the region file `(rx, rz)` overlaps this rectangle.
    /// Spans are computed in `i64`: `32 * rx` overflows `i32` for absurd
    /// region coordinates.
    pub const fn overlaps_region(self, rx: i32, rz: i32) -> bool {
        let rx0 = rx as i64 * 32;
        let rz0 = rz as i64 * 32;
        self.x0 as i64 <= rx0 + 31
            && rx0 <= self.x1 as i64
            && self.z0 as i64 <= rz0 + 31
            && rz0 <= self.z1 as i64
    }
}

/// One dimension's selected area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Area {
    /// Every chunk of the dimension.
    All,
    /// Chunk rectangle.
    Rect(Rect),
    /// Explicit chunk coordinates.
    Chunks(Vec<ChunkCoord>),
}

impl Area {
    fn contains(&self, coord: ChunkCoord) -> bool {
        match self {
            Self::All => true,
            Self::Rect(rect) => rect.contains(coord.x, coord.z),
            Self::Chunks(chunks) => chunks.contains(&coord),
        }
    }

    fn matches_region(&self, key: RegionKey) -> bool {
        match self {
            Self::All => true,
            Self::Rect(rect) => rect.overlaps_region(key.rx, key.rz),
            Self::Chunks(chunks) => chunks
                .iter()
                .any(|coord| coord.region_x() == key.rx && coord.region_z() == key.rz),
        }
    }
}

/// Portion of the world an operation may read or write. Coordinates outside
/// the scope are invisible: scoped backups record no tombstones for them
/// and scoped rollbacks never touch their files.
///
/// An empty [`Scope::Select`] matches nothing; use [`Scope::World`] for
/// the whole world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Every dimension, kind, and chunk.
    World,
    /// Per-dimension areas with uniformly applied region kinds.
    Select {
        /// Region families the selection covers.
        kinds: Vec<RegionKind>,
        /// One entry per selected dimension; unlisted dimensions are out.
        areas: Vec<(Dimension, Area)>,
    },
}

impl Scope {
    /// All three region families, for selections that do not narrow kinds.
    pub fn all_kinds() -> Vec<RegionKind> {
        alloc::vec![RegionKind::REGION, RegionKind::ENTITIES, RegionKind::POI,]
    }

    /// Whole dimension in all region families.
    pub fn dimension(dim: Dimension) -> Self {
        Self::Select {
            kinds: Self::all_kinds(),
            areas: alloc::vec![(dim, Area::All)],
        }
    }

    /// Whether a chunk coordinate falls inside the scope.
    pub fn contains(&self, coord: ChunkCoord) -> bool {
        match self {
            Self::World => true,
            Self::Select { kinds, areas } => {
                kinds.contains(&coord.kind)
                    && areas
                        .iter()
                        .any(|(dim, area)| *dim == coord.dim && area.contains(coord))
            }
        }
    }

    /// Whether a region file falls inside the scope.
    pub fn matches_region(&self, key: RegionKey) -> bool {
        match self {
            Self::World => true,
            Self::Select { kinds, areas } => {
                kinds.contains(&key.kind)
                    && areas
                        .iter()
                        .any(|(dim, area)| *dim == key.dim && area.matches_region(key))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RegionKind;

    const OVER: Dimension = Dimension::OVERWORLD;
    const NETHER: Dimension = Dimension::NETHER;
    const REGION: RegionKind = RegionKind::REGION;
    const ENTITIES: RegionKind = RegionKind::ENTITIES;

    #[test]
    fn world_matches_everything() {
        let scope = Scope::World;
        assert!(scope.contains(ChunkCoord::new(NETHER, REGION, -1, -1)));
        assert!(scope.matches_region(RegionKey::new(NETHER, REGION, -1, -1)));
    }

    #[test]
    fn dimension_selects_namespace_only() {
        let scope = Scope::dimension(NETHER);
        assert!(scope.contains(ChunkCoord::new(NETHER, REGION, 0, 0)));
        assert!(!scope.contains(ChunkCoord::new(OVER, REGION, 0, 0)));
        assert!(scope.matches_region(RegionKey::new(NETHER, REGION, 0, 0)));
        assert!(!scope.matches_region(RegionKey::new(OVER, REGION, 0, 0)));
    }

    #[test]
    fn rect_normalizes_and_matches() {
        let rect = Rect::new(31, 5, 0, -3);
        assert_eq!(
            rect,
            Rect {
                x0: 0,
                z0: -3,
                x1: 31,
                z1: 5
            }
        );
        assert!(rect.contains(0, 0));
        assert!(rect.contains(31, 5));
        assert!(!rect.contains(32, 0));
        assert!(!rect.contains(0, 6));
        assert!(rect.overlaps_region(0, 0));
        assert!(rect.overlaps_region(0, -1));
        assert!(!rect.overlaps_region(1, 0));
        assert!(!rect.overlaps_region(0, 1));
        assert!(Rect::region(-1, 2).contains(-32, 95));
        assert!(Rect::region(-1, 2).contains(-1, 64));
        assert!(!Rect::region(-1, 2).contains(0, 64));
    }

    #[test]
    fn chunks_match_list_and_owning_regions() {
        let listed = alloc::vec![
            ChunkCoord::new(OVER, REGION, 0, 0),
            ChunkCoord::new(OVER, REGION, 33, 0),
        ];
        let scope = Scope::Select {
            kinds: alloc::vec![REGION],
            areas: alloc::vec![(OVER, Area::Chunks(listed))],
        };
        assert!(scope.contains(ChunkCoord::new(OVER, REGION, 0, 0)));
        assert!(!scope.contains(ChunkCoord::new(OVER, REGION, 1, 0)));
        assert!(scope.matches_region(RegionKey::new(OVER, REGION, 0, 0)));
        assert!(scope.matches_region(RegionKey::new(OVER, REGION, 1, 0)));
        assert!(!scope.matches_region(RegionKey::new(OVER, REGION, 2, 0)));
        assert!(!scope.matches_region(RegionKey::new(NETHER, REGION, 0, 0)));
    }

    #[test]
    fn kinds_apply_uniformly() {
        let scope = Scope::Select {
            kinds: alloc::vec![ENTITIES],
            areas: alloc::vec![(OVER, Area::All)],
        };
        assert!(scope.contains(ChunkCoord::new(OVER, ENTITIES, 0, 0)));
        assert!(!scope.contains(ChunkCoord::new(OVER, REGION, 0, 0)));
        assert!(!scope.matches_region(RegionKey::new(OVER, REGION, 0, 0)));
    }

    #[test]
    fn empty_select_matches_nothing() {
        let scope = Scope::Select {
            kinds: Scope::all_kinds(),
            areas: alloc::vec![],
        };
        assert!(!scope.contains(ChunkCoord::new(OVER, REGION, 0, 0)));
        assert!(!scope.matches_region(RegionKey::new(OVER, REGION, 0, 0)));
    }
}
