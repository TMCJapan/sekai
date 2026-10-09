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
    ///
    /// Region coordinates whose chunk span leaves `i32` are clamped, so the
    /// rectangle stays consistent with [`Rect::overlaps_region`] instead of
    /// selecting nothing.
    pub const fn region(rx: i32, rz: i32) -> Self {
        Self {
            x0: region_first(rx),
            z0: region_first(rz),
            x1: region_last(rx),
            z1: region_last(rz),
        }
    }

    /// Whether a chunk coordinate falls inside the rectangle.
    pub const fn contains(self, x: i32, z: i32) -> bool {
        self.x0 <= x && x <= self.x1 && self.z0 <= z && z <= self.z1
    }

    /// Whether the region file `(rx, rz)` overlaps this rectangle.
    ///
    /// Spans come from the same clamping as [`Rect::region`], so the two
    /// always agree: `32 * rx` would overflow `i32` for absurd coordinates.
    pub const fn overlaps_region(self, rx: i32, rz: i32) -> bool {
        self.x0 <= region_last(rx)
            && region_first(rx) <= self.x1
            && self.z0 <= region_last(rz)
            && region_first(rz) <= self.z1
    }
}

/// Largest region coordinate whose 32-chunk span still fits `i32`.
const MAX_REGION: i32 = i32::MAX / 32;
/// Smallest region coordinate whose 32-chunk span still fits `i32`.
const MIN_REGION: i32 = i32::MIN / 32;

/// Clamp a region coordinate into the chunk-representable range.
const fn clamp_region(r: i32) -> i32 {
    if r > MAX_REGION {
        MAX_REGION
    } else if r < MIN_REGION {
        MIN_REGION
    } else {
        r
    }
}

/// First chunk coordinate of region file `r`.
const fn region_first(r: i32) -> i32 {
    clamp_region(r) * 32
}

/// Last chunk coordinate of region file `r`.
const fn region_last(r: i32) -> i32 {
    region_first(r).saturating_add(31)
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
    fn contains(&self, coord: &ChunkCoord) -> bool {
        match self {
            Self::All => true,
            Self::Rect(rect) => rect.contains(coord.x, coord.z),
            Self::Chunks(chunks) => chunks.contains(coord),
        }
    }

    fn matches_region(&self, key: &RegionKey) -> bool {
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
/// the whole world, or [`Scope::Kinds`] to narrow the whole world to
/// region families.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Every dimension, kind, and chunk.
    World,
    /// Every dimension, restricted to these region families.
    Kinds(Vec<RegionKind>),
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
    pub fn contains(&self, coord: &ChunkCoord) -> bool {
        match self {
            Self::World => true,
            Self::Kinds(kinds) => kinds.contains(&coord.kind),
            Self::Select { kinds, areas } => {
                kinds.contains(&coord.kind)
                    && areas
                        .iter()
                        .any(|(dim, area)| *dim == coord.dim && area.contains(coord))
            }
        }
    }

    /// Whether a region file falls inside the scope.
    pub fn matches_region(&self, key: &RegionKey) -> bool {
        match self {
            Self::World => true,
            Self::Kinds(kinds) => kinds.contains(&key.kind),
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
    const END: Dimension = Dimension::END;
    const REGION: RegionKind = RegionKind::REGION;
    const ENTITIES: RegionKind = RegionKind::ENTITIES;
    const POI_REGION: RegionKind = RegionKind::POI;

    #[test]
    fn world_matches_everything() {
        let scope = Scope::World;
        assert!(scope.contains(&ChunkCoord::new(NETHER, REGION, -1, -1)));
        assert!(scope.matches_region(&RegionKey::new(NETHER, REGION, -1, -1)));
    }

    #[test]
    fn dimension_selects_key_only() {
        let scope = Scope::dimension(NETHER);
        assert!(scope.contains(&ChunkCoord::new(NETHER, REGION, 0, 0)));
        assert!(!scope.contains(&ChunkCoord::new(OVER, REGION, 0, 0)));
        assert!(scope.matches_region(&RegionKey::new(NETHER, REGION, 0, 0)));
        assert!(!scope.matches_region(&RegionKey::new(OVER, REGION, 0, 0)));
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

    /// A region rectangle and its overlap test must agree even where the
    /// region spans chunks `i32` cannot name, or `--region` would silently
    /// select nothing.
    #[test]
    fn region_rectangles_agree_with_overlap_at_the_range_edges() {
        for r in [0, 1, -1, 1_000_000, -1_000_000, 67_108_863, -67_108_864] {
            let rect = Rect::region(r, r);
            assert!(rect.overlaps_region(r, r), "region {r} must overlap itself");
            assert!(rect.contains(rect.x0, rect.z0));
            assert!(rect.contains(rect.x1, rect.z1));
        }
        // Beyond the representable chunk range the span clamps, but the
        // region still selects itself instead of matching nothing.
        let far = i32::MAX;
        assert!(Rect::region(far, far).overlaps_region(far, far));
        let far_negative = i32::MIN;
        assert!(
            Rect::region(far_negative, far_negative).overlaps_region(far_negative, far_negative)
        );
        // Ordinary regions still exclude their neighbors.
        assert!(!Rect::region(0, 0).overlaps_region(1, 0));
        assert!(!Rect::region(0, 0).overlaps_region(0, 1));
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
        assert!(scope.contains(&ChunkCoord::new(OVER, REGION, 0, 0)));
        assert!(!scope.contains(&ChunkCoord::new(OVER, REGION, 1, 0)));
        assert!(scope.matches_region(&RegionKey::new(OVER, REGION, 0, 0)));
        assert!(scope.matches_region(&RegionKey::new(OVER, REGION, 1, 0)));
        assert!(!scope.matches_region(&RegionKey::new(OVER, REGION, 2, 0)));
        assert!(!scope.matches_region(&RegionKey::new(NETHER, REGION, 0, 0)));
    }

    #[test]
    fn kinds_apply_uniformly() {
        let scope = Scope::Select {
            kinds: alloc::vec![ENTITIES],
            areas: alloc::vec![(OVER, Area::All)],
        };
        assert!(scope.contains(&ChunkCoord::new(OVER, ENTITIES, 0, 0)));
        assert!(!scope.contains(&ChunkCoord::new(OVER, REGION, 0, 0)));
        assert!(!scope.matches_region(&RegionKey::new(OVER, REGION, 0, 0)));
    }

    #[test]
    fn kinds_scope_covers_every_dimension() {
        let scope = Scope::Kinds(alloc::vec![ENTITIES]);
        assert!(scope.contains(&ChunkCoord::new(NETHER, ENTITIES, 7, -7)));
        assert!(!scope.contains(&ChunkCoord::new(OVER, REGION, 0, 0)));
        assert!(scope.matches_region(&RegionKey::new(END, ENTITIES, 0, 0)));
        assert!(!scope.matches_region(&RegionKey::new(OVER, POI_REGION, 0, 0)));
        // An empty family list selects nothing rather than everything.
        let empty = Scope::Kinds(alloc::vec![]);
        assert!(!empty.contains(&ChunkCoord::new(OVER, REGION, 0, 0)));
        assert!(!empty.matches_region(&RegionKey::new(OVER, REGION, 0, 0)));
    }

    #[test]
    fn empty_select_matches_nothing() {
        let scope = Scope::Select {
            kinds: Scope::all_kinds(),
            areas: alloc::vec![],
        };
        assert!(!scope.contains(&ChunkCoord::new(OVER, REGION, 0, 0)));
        assert!(!scope.matches_region(&RegionKey::new(OVER, REGION, 0, 0)));
    }
}
