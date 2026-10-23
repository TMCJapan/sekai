//! Named, movable pointers to commits.
//!
//! Refs are the store's branch model: a name bound to a commit that moves
//! forward when a new commit lands on it. A ref without a commit is
//! *unborn* - it names a history that has not started yet. At most one ref
//! is *active*: the one new commits hang from. A store normally has
//! exactly one, but the invariant is "at most", enforced by the backend.
//!
//! An unborn ref can be active (the fresh-store state) or inactive (a name
//! waiting for its first commit).

use alloc::string::String;
use core::{fmt, str::FromStr};

use crate::commit::CommitId;
use crate::error::RefNameError;

/// Name of the ref a fresh store starts with: the active one, unborn,
/// waiting for the first commit.
pub const DEFAULT_REF_NAME: &str = "main";

/// Validated ref name.
///
/// Ref names are path-like (`main`, `feature/nightly`): 1-64 bytes of
/// ASCII alphanumerics plus `.`, `_`, `-`, and `/`, with no empty path
/// component (no leading, trailing, or doubled `/`) and no `.` or `..`
/// segment. A name is never all digits (so it cannot be mistaken for a
/// commit ID) and never `HEAD` (the active ref is the store's HEAD
/// equivalent).
///
/// Ordering and uniqueness are byte-wise and case-sensitive, so `main`
/// and `Main` are distinct refs; only the `HEAD` reservation ignores ASCII
/// case.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RefName(String);

impl RefName {
    /// Longest accepted name, in bytes (names are ASCII-only).
    pub const MAX_LEN: usize = 64;

    /// Name the store keeps for itself: no ref may carry it.
    const RESERVED: &str = "HEAD";

    /// Validate `raw` into a ref name.
    ///
    /// A name is ASCII-only: any other character fails with
    /// [`RefNameError::BadChar`]. The byte-length check runs first, so an
    /// over-long name reports [`RefNameError::TooLong`] even when it also
    /// holds a non-ASCII character.
    pub fn parse(raw: &str) -> Result<Self, RefNameError> {
        if raw.is_empty() {
            return Err(RefNameError::Empty);
        }
        if raw.len() > Self::MAX_LEN {
            return Err(RefNameError::TooLong(Self::MAX_LEN));
        }
        let mut all_digits = true;
        // Walk characters, not bytes: `BadChar` must name the rejected
        // character itself, not the first byte of its UTF-8 encoding.
        for c in raw.chars() {
            let valid = c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/');
            if !valid {
                return Err(RefNameError::BadChar(c));
            }
            all_digits &= c.is_ascii_digit();
        }
        if all_digits {
            return Err(RefNameError::Numeric);
        }
        // Path-like names: no empty component, no leading or trailing
        // separator, and no current- or parent-directory segment.
        if raw.starts_with('/') || raw.ends_with('/') || raw.contains("//") {
            return Err(RefNameError::BadPath);
        }
        if raw.split('/').any(|segment| matches!(segment, "." | "..")) {
            return Err(RefNameError::BadPath);
        }
        if raw.eq_ignore_ascii_case(Self::RESERVED) {
            return Err(RefNameError::Reserved);
        }
        Ok(Self(String::from(raw)))
    }

    /// Borrow the validated name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RefName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for RefName {
    type Err = RefNameError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

/// A named pointer to a commit.
///
/// The record carries the active flag but cannot enforce the store-wide
/// invariant on its own: a store holds at most one ref with `active` set,
/// and the backend maintains that when the flag moves (`set_ref` and
/// `activate_ref`). An active ref may be unborn; that is the fresh-store
/// state. The flag is advisory on a value: a hand-built record is not the
/// store's authority on which ref is active, so callers must not treat it
/// as one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRef {
    /// Ref name.
    pub name: RefName,
    /// Commit the ref points at, or `None` while the ref is unborn.
    pub commit: Option<CommitId>,
    /// Whether this ref is the active one (new commits hang from it).
    pub active: bool,
    /// Creation time as Unix millis (informational only).
    pub created_at_ms: u64,
}

impl CommitRef {
    /// Construct a ref record.
    pub const fn new(
        name: RefName,
        commit: Option<CommitId>,
        active: bool,
        created_at_ms: u64,
    ) -> Self {
        Self {
            name,
            commit,
            active,
            created_at_ms,
        }
    }

    /// Whether the ref names no commit yet.
    pub const fn is_unborn(&self) -> bool {
        self.commit.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn accepts_path_like_names() {
        for raw in [
            "main",
            "feature/nightly",
            "a",
            "x-y.z_w",
            "v..1",
            "a..b",
            "release..2024",
        ] {
            assert_eq!(RefName::parse(raw).unwrap().as_str(), raw);
        }
        assert_eq!(RefName::from_str("main").unwrap().to_string(), "main");
    }

    #[test]
    fn rejects_bad_names() {
        assert_eq!(RefName::parse(""), Err(RefNameError::Empty));
        assert_eq!(RefName::parse("123"), Err(RefNameError::Numeric));
        assert_eq!(RefName::parse("a b"), Err(RefNameError::BadChar(' ')));
        assert_eq!(RefName::parse("@x"), Err(RefNameError::BadChar('@')));
        // A multi-byte character is reported as itself, not as one of its
        // bytes.
        assert_eq!(RefName::parse("bré"), Err(RefNameError::BadChar('é')));
        assert_eq!(RefName::parse("/main"), Err(RefNameError::BadPath));
        assert_eq!(RefName::parse("main/"), Err(RefNameError::BadPath));
        assert_eq!(RefName::parse("a//b"), Err(RefNameError::BadPath));
        assert_eq!(RefName::parse(".."), Err(RefNameError::BadPath));
        assert_eq!(RefName::parse("a/../b"), Err(RefNameError::BadPath));
        assert_eq!(RefName::parse("a/.."), Err(RefNameError::BadPath));
        assert_eq!(RefName::parse("."), Err(RefNameError::BadPath));
        assert_eq!(RefName::parse("a/./b"), Err(RefNameError::BadPath));
        assert_eq!(RefName::parse("a/."), Err(RefNameError::BadPath));
        assert_eq!(RefName::parse("HEAD"), Err(RefNameError::Reserved));
        assert_eq!(RefName::parse("head"), Err(RefNameError::Reserved));
        let long = "a".repeat(RefName::MAX_LEN + 1);
        assert_eq!(
            RefName::parse(&long),
            Err(RefNameError::TooLong(RefName::MAX_LEN))
        );
        // The byte-length check runs first: an over-long name reports
        // `TooLong` even when it also holds a non-ASCII character.
        let long_non_ascii = "é".repeat(RefName::MAX_LEN);
        assert_eq!(
            RefName::parse(&long_non_ascii),
            Err(RefNameError::TooLong(RefName::MAX_LEN))
        );
        assert!(RefName::parse(&"a".repeat(RefName::MAX_LEN)).is_ok());
    }

    #[test]
    fn unborn_refs_have_no_commit() {
        let name = RefName::parse("main").unwrap();
        let record = CommitRef::new(name.clone(), None, true, 1_700_000_000_000);
        assert!(record.is_unborn());
        assert!(record.active);
        assert_eq!(record.name, name);
    }
}
