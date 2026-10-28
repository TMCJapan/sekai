//! Named scopes: store-saved world areas selected by name (ADR-0021).
//!
//! A named scope describes a part of the world, not a position in its
//! history: it names no commit, and resolving it yields the same
//! [`Scope::Select`] an inline selection of its areas would.

use alloc::string::String;
use alloc::vec::Vec;
use core::{fmt, str::FromStr};

use crate::error::{NamedScopeError, ScopeNameError};
use crate::name::{self, NameRule};
use crate::{Area, Dimension, Rect, Scope};

/// Validated scope name.
///
/// Scope names follow [`RefName`](crate::RefName)'s path grammar (1-64
/// bytes of ASCII alphanumerics plus `.`, `_`, `-`, and `/`, no empty or
/// traversing path component) but reserve no name and may be all digits:
/// no command reads a scope name where a commit ID could stand. The
/// separate type keeps scope and ref names from being swapped.
///
/// Ordering and uniqueness are byte-wise and case-sensitive.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ScopeName(String);

impl ScopeName {
    /// Longest accepted name, in bytes (names are ASCII-only).
    pub const MAX_LEN: usize = name::MAX_LEN;

    /// Validate `raw` into a scope name.
    ///
    /// The byte-length check runs first, so an over-long name reports
    /// [`ScopeNameError::TooLong`] even when it also holds a non-ASCII
    /// character.
    pub fn parse(raw: &str) -> Result<Self, ScopeNameError> {
        name::check(raw).map_err(|rule| match rule {
            NameRule::Empty => ScopeNameError::Empty,
            NameRule::TooLong => ScopeNameError::TooLong(Self::MAX_LEN),
            NameRule::BadChar(c) => ScopeNameError::BadChar(c),
            NameRule::BadPath(reason) => ScopeNameError::BadPath(reason),
        })?;
        Ok(Self(String::from(raw)))
    }

    /// Borrow the validated name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ScopeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ScopeName {
    type Err = ScopeNameError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

/// A store-saved selection: chunk rectangles over every region family.
///
/// A scope is purely geographic: it always covers every region family, so
/// it stores no kind subset (ADR-0021).
///
/// The areas are private so every record holds exactly one spelling of
/// its selection, matching the store's: sorted by `(dim, x0, z0, x1, z1)`
/// with duplicates collapsed. Overlapping areas are kept apart; the scope
/// is their union either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedScope {
    /// Scope name.
    pub name: ScopeName,
    areas: Vec<(Dimension, Rect)>,
    /// Creation time as Unix millis (informational only).
    pub created_at_ms: u64,
}

impl NamedScope {
    /// Build a record, normalizing its spelling. A record without an area
    /// would select nothing and is refused.
    ///
    /// Areas are re-normalized through [`Rect::new`]: `Rect`'s fields are
    /// public, so a struct literal can put a maximum before its minimum,
    /// which a store would refuse or keep depending on its backend.
    pub fn new(
        name: ScopeName,
        areas: Vec<(Dimension, Rect)>,
        created_at_ms: u64,
    ) -> Result<Self, NamedScopeError> {
        if areas.is_empty() {
            return Err(NamedScopeError::NoAreas);
        }
        let mut areas: Vec<(Dimension, Rect)> = areas
            .into_iter()
            .map(|(dim, rect)| (dim, Rect::new(rect.x0, rect.z0, rect.x1, rect.z1)))
            .collect();
        areas.sort_unstable();
        areas.dedup();
        Ok(Self {
            name,
            areas,
            created_at_ms,
        })
    }

    /// Selected areas in `(dim, x0, z0, x1, z1)` order.
    pub fn areas(&self) -> &[(Dimension, Rect)] {
        &self.areas
    }

    /// The selection this record names: every region family, one
    /// rectangle per area.
    pub fn to_scope(&self) -> Scope {
        Scope::Select {
            kinds: Scope::all_kinds(),
            areas: self
                .areas
                .iter()
                .map(|&(dim, rect)| (dim, Area::Rect(rect)))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChunkCoord, RegionKey, RegionKind};
    use alloc::string::ToString;
    use alloc::vec;

    const OVER: Dimension = Dimension::OVERWORLD;
    const NETHER: Dimension = Dimension::NETHER;

    fn name(raw: &str) -> ScopeName {
        ScopeName::parse(raw).unwrap()
    }

    #[test]
    fn scope_names_follow_the_ref_grammar_without_reservations() {
        for raw in ["spawn", "hub/nether", "x-y.z_w", "1/2", "HEAD", "42"] {
            assert_eq!(ScopeName::parse(raw).unwrap().as_str(), raw);
        }
        assert_eq!(ScopeName::from_str("spawn").unwrap().to_string(), "spawn");
        assert_eq!(ScopeName::parse(""), Err(ScopeNameError::Empty));
        assert_eq!(ScopeName::parse("a b"), Err(ScopeNameError::BadChar(' ')));
        assert_eq!(
            ScopeName::parse("a//b"),
            Err(ScopeNameError::BadPath("empty path component"))
        );
        assert_eq!(
            ScopeName::parse("a/.."),
            Err(ScopeNameError::BadPath("'.' or '..' segment"))
        );
        assert_eq!(
            ScopeName::parse(&"a".repeat(ScopeName::MAX_LEN + 1)),
            Err(ScopeNameError::TooLong(ScopeName::MAX_LEN))
        );
        assert_eq!(
            ScopeName::parse(&"é".repeat(ScopeName::MAX_LEN)),
            Err(ScopeNameError::TooLong(ScopeName::MAX_LEN)),
            "the length check runs before the character walk"
        );
        assert_eq!(
            ScopeNameError::Empty.to_string(),
            "scope name is empty",
            "errors name the scope, not a ref"
        );
    }

    #[test]
    fn records_hold_one_spelling() {
        let a = Rect::new(4, 4, 0, 0);
        let b = Rect::new(-8, 0, -1, 3);
        let record = NamedScope::new(
            name("spawn"),
            vec![(NETHER, a), (OVER, a), (OVER, b), (OVER, a)],
            1,
        )
        .unwrap();
        assert_eq!(record.areas(), &[(OVER, b), (OVER, a), (NETHER, a)]);
    }

    #[test]
    fn inverted_rectangles_are_renormalized() {
        let inverted = Rect {
            x0: 3,
            z0: 5,
            x1: -1,
            z1: 2,
        };
        let record = NamedScope::new(name("spawn"), vec![(OVER, inverted)], 1).unwrap();
        assert_eq!(record.areas(), &[(OVER, Rect::new(-1, 2, 3, 5))]);
    }

    #[test]
    fn records_without_areas_are_refused() {
        assert_eq!(
            NamedScope::new(name("spawn"), Vec::new(), 1),
            Err(NamedScopeError::NoAreas)
        );
    }

    #[test]
    fn records_resolve_to_the_union_of_their_areas_in_every_family() {
        let record = NamedScope::new(
            name("spawn"),
            vec![
                (OVER, Rect::new(0, 0, 0, 0)),
                (NETHER, Rect::region(-1, -1)),
            ],
            1,
        )
        .unwrap();
        let scope = record.to_scope();
        for kind in RegionKind::ALL {
            assert!(scope.contains(ChunkCoord::new(OVER, kind, 0, 0)));
            assert!(!scope.contains(ChunkCoord::new(OVER, kind, 1, 0)));
            assert!(scope.covers_region(RegionKey::new(NETHER, kind, -1, -1)));
        }
    }
}
