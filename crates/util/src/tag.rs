//! Validated snapshot tag names.
//!
//! Tags give snapshots human-readable aliases (`@before-update`). Names
//! are restricted to `[A-Za-z0-9._-]` so they survive shells, paths, and
//! JSON without quoting, and never consist of digits alone so `@123`
//! cannot be confused with a snapshot ID.

use alloc::string::String;
use core::{fmt, str::FromStr};

use crate::error::TagNameError;

/// Validated snapshot tag name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TagName(String);

impl TagName {
    /// Longest accepted name, in bytes (names are ASCII-only).
    pub const MAX_LEN: usize = 64;

    /// Validate `raw` into a tag name.
    pub fn parse(raw: &str) -> Result<Self, TagNameError> {
        if raw.is_empty() {
            return Err(TagNameError::Empty);
        }
        if raw.len() > Self::MAX_LEN {
            return Err(TagNameError::TooLong);
        }
        let mut all_digits = true;
        for byte in raw.bytes() {
            let valid = byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-');
            if !valid {
                return Err(TagNameError::BadChar(byte as char));
            }
            all_digits &= byte.is_ascii_digit();
        }
        if all_digits {
            return Err(TagNameError::Numeric);
        }
        Ok(Self(String::from(raw)))
    }

    /// Borrow the validated name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TagName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for TagName {
    type Err = TagNameError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn accepts_sane_names() {
        for raw in ["v1", "before-update_26.1", "a", "x-y.z_w"] {
            assert_eq!(TagName::parse(raw).unwrap().as_str(), raw);
        }
        assert_eq!(TagName::from_str("v1").unwrap().to_string(), "v1");
    }

    #[test]
    fn rejects_bad_names() {
        assert_eq!(TagName::parse(""), Err(TagNameError::Empty));
        assert_eq!(TagName::parse("123"), Err(TagNameError::Numeric));
        assert_eq!(TagName::parse("a b"), Err(TagNameError::BadChar(' ')));
        assert_eq!(TagName::parse("@x"), Err(TagNameError::BadChar('@')));
        assert_eq!(TagName::parse(" слоvo"), Err(TagNameError::BadChar(' ')));
        let long = "a".repeat(TagName::MAX_LEN + 1);
        assert_eq!(TagName::parse(&long), Err(TagNameError::TooLong));
        assert!(TagName::parse("a".repeat(TagName::MAX_LEN).as_str()).is_ok());
    }
}
