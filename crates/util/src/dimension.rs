//! Dimension identity keys.
//!
//! Every dimension is identified by a stable string key. Official
//! dimensions use their namespaced id (`minecraft:overworld`,
//! `aether:sky`), which the game guarantees unique and which survives
//! folder moves and layout migrations. Layout-derived dimensions -
//! arbitrary world folders inside a scanned tree - have no official id,
//! so they use their root-relative folder path with a `./` prefix
//! (`./sky`, `./sky/DIM-1`); the prefix keeps them disjoint from
//! namespaced ids by construction.

use alloc::borrow::Cow;
use alloc::format;
use alloc::string::String;
use core::fmt;
use core::str::FromStr;

use crate::error::DimensionError;

/// Identity of one dimension.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Dimension(Cow<'static, str>);

impl Dimension {
    /// Vanilla overworld (`minecraft:overworld`).
    pub const OVERWORLD: Self = Self(Cow::Borrowed("minecraft:overworld"));
    /// Vanilla nether (`minecraft:the_nether`).
    pub const NETHER: Self = Self(Cow::Borrowed("minecraft:the_nether"));
    /// Vanilla end (`minecraft:the_end`).
    pub const END: Self = Self(Cow::Borrowed("minecraft:the_end"));

    /// Wrap a canonical key.
    pub fn new(key: impl Into<Cow<'static, str>>) -> Self {
        Self(key.into())
    }

    /// Official namespaced id from its parts (`namespace:path`).
    pub fn id(namespace: &str, path: &str) -> Self {
        Self(Cow::Owned(format!("{namespace}:{path}")))
    }

    /// Layout-derived key for a root-relative world-folder path.
    pub fn folder(relative: &str) -> Self {
        Self(Cow::Owned(format!("./{relative}")))
    }

    /// Canonical key as stored and displayed.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Namespace of a namespaced id; `None` for folder keys.
    pub fn namespace(&self) -> Option<&str> {
        self.as_str()
            .split_once(':')
            .map(|(namespace, _)| namespace)
    }

    /// Path portion of a namespaced id, or the folder path without `./`.
    pub fn path(&self) -> &str {
        self.as_str()
            .split_once(':')
            .map_or_else(|| self.as_str().trim_start_matches("./"), |(_, path)| path)
    }

    /// Parse user-facing text: a vanilla alias, a namespaced id, or a
    /// `./`-prefixed folder key.
    ///
    /// Namespaced ids are validated structurally (exactly one colon, both
    /// parts non-empty) but not against the game's character set: a folder
    /// the game refuses to load is still backed up and must stay
    /// selectable. Bare names are only vanilla aliases - folder keys always
    /// carry the `./` prefix, so a typo never silently selects nothing.
    pub fn parse(s: &str) -> Result<Self, DimensionError> {
        if s.is_empty() {
            return Err(DimensionError::Empty);
        }
        if let Some(relative) = s.strip_prefix("./") {
            validate_folder(relative)?;
            return Ok(Self(Cow::Owned(String::from(s))));
        }
        if let Some((namespace, path)) = s.split_once(':') {
            if namespace.is_empty() || path.is_empty() || path.contains(':') {
                return Err(DimensionError::Invalid);
            }
            return Ok(Self(Cow::Owned(String::from(s))));
        }
        match s.to_ascii_lowercase().as_str() {
            "ow" | "overworld" => Ok(Self::OVERWORLD),
            "ne" | "nether" | "the_nether" | "dim-1" => Ok(Self::NETHER),
            "end" | "the_end" | "dim1" => Ok(Self::END),
            _ => Err(DimensionError::Invalid),
        }
    }
}

/// Folder keys are `/`-separated non-empty components, never absolute.
fn validate_folder(relative: &str) -> Result<(), DimensionError> {
    if relative.is_empty()
        || relative.starts_with('/')
        || relative.ends_with('/')
        || relative.split('/').any(str::is_empty)
    {
        return Err(DimensionError::Invalid);
    }
    Ok(())
}

impl FromStr for Dimension {
    type Err = DimensionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl fmt::Display for Dimension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use core::str::FromStr;

    #[test]
    fn parses_vanilla_aliases() {
        assert_eq!("overworld".parse(), Ok(Dimension::OVERWORLD));
        assert_eq!("ow".parse(), Ok(Dimension::OVERWORLD));
        assert_eq!("OVERWORLD".parse(), Ok(Dimension::OVERWORLD));
        assert_eq!("nether".parse(), Ok(Dimension::NETHER));
        assert_eq!("the_nether".parse(), Ok(Dimension::NETHER));
        assert_eq!("DIM-1".parse(), Ok(Dimension::NETHER));
        assert_eq!("end".parse(), Ok(Dimension::END));
        assert_eq!("the_end".parse(), Ok(Dimension::END));
        assert_eq!("DIM1".parse(), Ok(Dimension::END));
        // Full ids pass through, including non-vanilla namespaces.
        assert_eq!("minecraft:overworld".parse(), Ok(Dimension::OVERWORLD));
        assert_eq!("aether:sky".parse(), Ok(Dimension::new("aether:sky")));
    }

    #[test]
    fn parses_folder_keys() {
        assert_eq!("./sky".parse(), Ok(Dimension::folder("sky")));
        assert_eq!("./sky/DIM-1".parse(), Ok(Dimension::folder("sky/DIM-1")));
        // The `./` prefix keeps a colon-bearing folder name unambiguous.
        assert_eq!("./a:b".parse(), Ok(Dimension::folder("a:b")));
    }

    #[test]
    fn rejects_malformed_keys() {
        assert_eq!(Dimension::from_str(""), Err(DimensionError::Empty));
        assert_eq!(Dimension::from_str("mordor"), Err(DimensionError::Invalid));
        assert_eq!(Dimension::from_str("42"), Err(DimensionError::Invalid));
        assert_eq!(Dimension::from_str(":"), Err(DimensionError::Invalid));
        assert_eq!(Dimension::from_str("a:"), Err(DimensionError::Invalid));
        assert_eq!(Dimension::from_str(":b"), Err(DimensionError::Invalid));
        assert_eq!(Dimension::from_str("a:b:c"), Err(DimensionError::Invalid));
        assert_eq!(Dimension::from_str("./"), Err(DimensionError::Invalid));
        assert_eq!(Dimension::from_str(".//x"), Err(DimensionError::Invalid));
        assert_eq!(Dimension::from_str("./x/"), Err(DimensionError::Invalid));
    }

    #[test]
    fn displays_canonical_keys() {
        assert_eq!(Dimension::OVERWORLD.to_string(), "minecraft:overworld");
        assert_eq!(Dimension::NETHER.to_string(), "minecraft:the_nether");
        assert_eq!(Dimension::END.to_string(), "minecraft:the_end");
        assert_eq!(Dimension::folder("sky/DIM-1").to_string(), "./sky/DIM-1");
        // Display output round-trips through FromStr.
        for dim in [
            Dimension::OVERWORLD,
            Dimension::NETHER,
            Dimension::END,
            Dimension::new("aether:sky"),
            Dimension::folder("sky/DIM-1"),
        ] {
            assert_eq!(dim.to_string().parse(), Ok(dim));
        }
    }

    #[test]
    fn splits_namespace_and_path() {
        assert_eq!(Dimension::OVERWORLD.namespace(), Some("minecraft"));
        assert_eq!(Dimension::OVERWORLD.path(), "overworld");
        assert_eq!(Dimension::new("aether:sky/islands").path(), "sky/islands");
        assert_eq!(Dimension::folder("sky/DIM-1").namespace(), None);
        assert_eq!(Dimension::folder("sky/DIM-1").path(), "sky/DIM-1");
    }
}
