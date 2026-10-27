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
use crate::{Area, Dimension, Rect, RegionKind, Scope};

/// Validated scope name.
///
/// Scope names follow [`RefName`](crate::RefName)'s grammar (1-64 bytes of
/// ASCII alphanumerics plus `.`, `_`, `-`, and `/`, not all digits, no
/// empty or traversing path component) but reserve no name. The separate
/// type keeps scope and ref names from being swapped.
///
/// Ordering and uniqueness are byte-wise and case-sensitive.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
            NameRule::Numeric => ScopeNameError::Numeric,
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

/// A store-saved selection: chunk rectangles plus the region families they
/// cover.
///
/// The fields are private so every record holds exactly one spelling of
/// its selection, matching the store's: areas sorted by
/// `(dim, x0, z0, x1, z1)` with duplicates collapsed, and kinds either
/// `None` (every family) or a sorted, deduplicated proper subset. Overlapping
/// areas are kept apart; the scope is their union either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedScope {
    /// Scope name.
    pub name: ScopeName,
    kinds: Option<Vec<RegionKind>>,
    areas: Vec<(Dimension, Rect)>,
    /// Creation time as Unix millis (informational only).
    pub created_at_ms: u64,
}

impl NamedScope {
    /// Build a record, normalizing its spelling.
    ///
    /// `kinds = None` selects every region family, and so does a set
    /// naming all of them. A record that would select nothing - no area,
    /// or an empty kind set - is refused.
    pub fn new(
        name: ScopeName,
        kinds: Option<Vec<RegionKind>>,
        mut areas: Vec<(Dimension, Rect)>,
        created_at_ms: u64,
    ) -> Result<Self, NamedScopeError> {
        if areas.is_empty() {
            return Err(NamedScopeError::NoAreas);
        }
        areas.sort_unstable();
        areas.dedup();
        let kinds = match kinds {
            None => None,
            Some(mut kinds) => {
                kinds.sort_unstable();
                kinds.dedup();
                match kinds.len() {
                    0 => return Err(NamedScopeError::NoKinds),
                    n if n == RegionKind::ALL.len() => None,
                    _ => Some(kinds),
                }
            }
        };
        Ok(Self {
            name,
            kinds,
            areas,
            created_at_ms,
        })
    }

    /// Selected region families, or `None` for all of them.
    pub fn kinds(&self) -> Option<&[RegionKind]> {
        self.kinds.as_deref()
    }

    /// Selected areas in `(dim, x0, z0, x1, z1)` order.
    pub fn areas(&self) -> &[(Dimension, Rect)] {
        &self.areas
    }

    /// The selection this record names, one rectangle per area.
    pub fn to_scope(&self) -> Scope {
        Scope::Select {
            kinds: self.kinds.clone().unwrap_or_else(Scope::all_kinds),
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
    use crate::{ChunkCoord, RegionKey};
    use alloc::string::ToString;
    use alloc::vec;

    const OVER: Dimension = Dimension::OVERWORLD;
    const NETHER: Dimension = Dimension::NETHER;

    fn name(raw: &str) -> ScopeName {
        ScopeName::parse(raw).unwrap()
    }

    #[test]
    fn scope_names_follow_the_ref_grammar_without_reservations() {
        for raw in ["spawn", "hub/nether", "x-y.z_w", "1/2", "HEAD"] {
            assert_eq!(ScopeName::parse(raw).unwrap().as_str(), raw);
        }
        assert_eq!(ScopeName::from_str("spawn").unwrap().to_string(), "spawn");
        assert_eq!(ScopeName::parse(""), Err(ScopeNameError::Empty));
        assert_eq!(ScopeName::parse("42"), Err(ScopeNameError::Numeric));
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
            Some(vec![RegionKind::Poi, RegionKind::Region, RegionKind::Poi]),
            vec![(NETHER, a), (OVER, a), (OVER, b), (OVER, a)],
            1,
        )
        .unwrap();
        assert_eq!(record.areas(), &[(OVER, b), (OVER, a), (NETHER, a)]);
        assert_eq!(
            record.kinds(),
            Some(&[RegionKind::Region, RegionKind::Poi][..])
        );

        let every_kind = NamedScope::new(
            name("spawn"),
            Some(vec![
                RegionKind::Poi,
                RegionKind::Entities,
                RegionKind::Region,
            ]),
            vec![(OVER, a)],
            1,
        )
        .unwrap();
        assert_eq!(every_kind.kinds(), None, "the full set is spelled None");
        assert_eq!(
            every_kind,
            NamedScope::new(name("spawn"), None, vec![(OVER, a)], 1).unwrap()
        );
    }

    #[test]
    fn records_that_select_nothing_are_refused() {
        assert_eq!(
            NamedScope::new(name("spawn"), None, Vec::new(), 1),
            Err(NamedScopeError::NoAreas)
        );
        assert_eq!(
            NamedScope::new(
                name("spawn"),
                Some(Vec::new()),
                vec![(OVER, Rect::new(0, 0, 0, 0))],
                1
            ),
            Err(NamedScopeError::NoKinds)
        );
    }

    #[test]
    fn records_resolve_to_the_union_of_their_areas() {
        let record = NamedScope::new(
            name("spawn"),
            Some(vec![RegionKind::Entities]),
            vec![
                (OVER, Rect::new(0, 0, 0, 0)),
                (NETHER, Rect::region(-1, -1)),
            ],
            1,
        )
        .unwrap();
        let scope = record.to_scope();
        assert!(scope.contains(ChunkCoord::new(OVER, RegionKind::Entities, 0, 0)));
        assert!(!scope.contains(ChunkCoord::new(OVER, RegionKind::Region, 0, 0)));
        assert!(!scope.contains(ChunkCoord::new(OVER, RegionKind::Entities, 1, 0)));
        assert!(scope.covers_region(RegionKey::new(NETHER, RegionKind::Entities, -1, -1)));

        let all = NamedScope::new(name("spawn"), None, vec![(OVER, Rect::new(0, 0, 0, 0))], 1)
            .unwrap()
            .to_scope();
        for kind in RegionKind::ALL {
            assert!(all.contains(ChunkCoord::new(OVER, kind, 0, 0)));
        }
    }
}
