//! Backup/rollback/diff scope
//!
//! Whole world, one dimension, or an explicit chunk list. Borrowed slices
//! keep the caller-owned selection without copying; region membership for
//! chunk lists is derived per query.

use alloc::vec::Vec;

use super::{ChunkCoord, Dimension, RegionKey};

/// Portion of the world an operation may read or write. Coordinates outside
/// the scope are invisible: scoped backups record no tombstones for them
/// and scoped rollbacks never touch their files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope<'a> {
    /// Every dimension, kind, and chunk.
    World,
    /// One dimension namespace (all kinds and chunks within it).
    Dimension(Dimension),
    /// One region file (all chunks it owns).
    Region(RegionKey),
    /// Explicit chunk coordinates; a region belongs to the scope when it
    /// owns at least one listed chunk.
    Chunks(&'a [ChunkCoord]),
}

impl Scope<'_> {
    /// Whether a chunk coordinate falls inside the scope.
    pub fn contains(self, coord: ChunkCoord) -> bool {
        match self {
            Self::World => true,
            Self::Dimension(dim) => dim == coord.dim,
            Self::Region(key) => RegionKey::of(coord) == key,
            Self::Chunks(chunks) => chunks.contains(&coord),
        }
    }

    /// Whether a region file falls inside the scope.
    pub fn matches_region(self, key: RegionKey) -> bool {
        match self {
            Self::World => true,
            Self::Dimension(dim) => dim == key.dim,
            Self::Region(target) => target == key,
            Self::Chunks(chunks) => chunks.iter().any(|coord| RegionKey::of(*coord) == key),
        }
    }
}

/// Owned scope filter for `'static` task boundaries. [`Scope`] borrows the
/// caller slice, which cannot cross spawn boundaries; convert once at the
/// call site and move clones into workers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnedScope {
    /// Every dimension, kind, and chunk.
    World,
    /// One dimension namespace (all kinds and chunks within it).
    Dimension(Dimension),
    /// One region file (all chunks it owns).
    Region(RegionKey),
    /// Explicit chunk coordinates.
    Chunks(Vec<ChunkCoord>),
}

impl From<Scope<'_>> for OwnedScope {
    fn from(scope: Scope<'_>) -> Self {
        match scope {
            Scope::World => Self::World,
            Scope::Dimension(dim) => Self::Dimension(dim),
            Scope::Region(key) => Self::Region(key),
            Scope::Chunks(chunks) => Self::Chunks(chunks.to_vec()),
        }
    }
}

impl OwnedScope {
    /// Whether a chunk coordinate falls inside the scope.
    pub fn contains(&self, coord: ChunkCoord) -> bool {
        match self {
            Self::World => true,
            Self::Dimension(dim) => *dim == coord.dim,
            Self::Region(key) => RegionKey::of(coord) == *key,
            Self::Chunks(chunks) => chunks.contains(&coord),
        }
    }

    /// Whether a region file falls inside the scope.
    pub fn matches_region(&self, key: RegionKey) -> bool {
        match self {
            Self::World => true,
            Self::Dimension(dim) => *dim == key.dim,
            Self::Region(target) => *target == key,
            Self::Chunks(chunks) => chunks.iter().any(|coord| RegionKey::of(*coord) == key),
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

    #[test]
    fn world_matches_everything() {
        let scope = Scope::World;
        assert!(scope.contains(ChunkCoord::new(NETHER, REGION, -1, -1)));
        assert!(scope.matches_region(RegionKey::new(NETHER, REGION, -1, -1)));
    }

    #[test]
    fn dimension_filters_namespace_only() {
        let scope = Scope::Dimension(NETHER);
        assert!(scope.contains(ChunkCoord::new(NETHER, REGION, 0, 0)));
        assert!(!scope.contains(ChunkCoord::new(OVER, REGION, 0, 0)));
        assert!(scope.matches_region(RegionKey::new(NETHER, REGION, 0, 0)));
        assert!(!scope.matches_region(RegionKey::new(OVER, REGION, 0, 0)));
    }

    #[test]
    fn chunks_match_list_and_owning_regions() {
        let listed = [
            ChunkCoord::new(OVER, REGION, 0, 0),
            ChunkCoord::new(OVER, REGION, 33, 0),
        ];
        let scope = Scope::Chunks(&listed);
        assert!(scope.contains(listed[0]));
        assert!(!scope.contains(ChunkCoord::new(OVER, REGION, 1, 0)));
        assert!(scope.matches_region(RegionKey::new(OVER, REGION, 0, 0)));
        assert!(scope.matches_region(RegionKey::new(OVER, REGION, 1, 0)));
        assert!(!scope.matches_region(RegionKey::new(OVER, REGION, 2, 0)));
        assert!(!scope.matches_region(RegionKey::new(NETHER, REGION, 0, 0)));
        assert!(!Scope::Chunks(&[]).matches_region(RegionKey::new(OVER, REGION, 0, 0)));
    }

    #[test]
    fn region_matches_owning_file_only() {
        let scope = Scope::Region(RegionKey::new(OVER, REGION, 0, 0));
        assert!(scope.contains(ChunkCoord::new(OVER, REGION, 0, 0)));
        assert!(scope.contains(ChunkCoord::new(OVER, REGION, 31, 31)));
        assert!(!scope.contains(ChunkCoord::new(OVER, REGION, 32, 0)));
        assert!(!scope.contains(ChunkCoord::new(NETHER, REGION, 0, 0)));
        assert!(scope.matches_region(RegionKey::new(OVER, REGION, 0, 0)));
        assert!(!scope.matches_region(RegionKey::new(OVER, REGION, 1, 0)));
    }

    #[test]
    fn owned_scope_mirrors_borrowed() {
        for scope in [
            Scope::World,
            Scope::Dimension(NETHER),
            Scope::Region(RegionKey::new(OVER, REGION, 0, 0)),
            Scope::Chunks(&[ChunkCoord::new(OVER, REGION, 0, 0)]),
        ] {
            let owned = OwnedScope::from(scope);
            let coord = ChunkCoord::new(NETHER, REGION, 5, 5);
            assert_eq!(owned.contains(coord), scope.contains(coord));
            assert_eq!(
                owned.contains(ChunkCoord::new(OVER, REGION, 0, 0)),
                scope.contains(ChunkCoord::new(OVER, REGION, 0, 0))
            );
            let key = RegionKey::new(NETHER, REGION, 0, 0);
            assert_eq!(owned.matches_region(key), scope.matches_region(key));
        }
    }
}
