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
use crate::name;

/// Name of the ref a fresh store starts with: the active one, unborn,
/// waiting for the first commit.
pub const DEFAULT_REF_NAME: &str = "main";

/// Validated ref name.
///
/// Names are path-like (`main`, `feature/nightly`): 1-255 bytes of ASCII
/// alphanumerics plus `.`, `_`, `-`, and `/`, with no empty `/`-separated
/// component, no `..`, no trailing `.`, and no leading `-`.
/// Git refuses more shapes, such as a `.lock` suffix, which guard its
/// files backend; sekai keeps refs in a database and accepts them. Only
/// the exact name `HEAD` is reserved.
///
/// Ordering and uniqueness are byte-wise and case-sensitive, so `main`
/// and `Main` are distinct refs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RefName(String);

impl RefName {
    /// Longest accepted name, in bytes (names are ASCII-only). Git sets no
    /// limit.
    pub const MAX_LEN: usize = name::MAX_LEN;

    /// Name the store keeps for itself: no ref may carry it.
    pub const RESERVED: &str = "HEAD";

    /// Validate `raw` into a ref name.
    ///
    /// A name is ASCII-only: any other character fails with
    /// [`RefNameError::BadChar`]. The byte-length check runs first, so an
    /// over-long name reports [`RefNameError::TooLong`] even when it also
    /// holds a non-ASCII character.
    pub fn parse(raw: &str) -> Result<Self, RefNameError> {
        name::check(raw).map_err(RefNameError::from)?;
        // An empty component would break the ref hierarchy that refnames
        // and their short forms rely on.
        if raw.split('/').any(str::is_empty) {
            return Err(RefNameError::BadPath("empty path component"));
        }
        // `..` would read as the range in revision syntax such as `a..b`.
        if raw.contains("..") {
            return Err(RefNameError::BadPath("contains '..'"));
        }
        // A name ending in `.` would fuse with `..b` into `a...b`,
        // the unsupported symmetric difference. Git refuses it too.
        if raw.ends_with('.') {
            return Err(RefNameError::BadPath("ends with '.'"));
        }
        // Git accepts such a ref but refuses to create one, as it would
        // read as an option; refusing it here means no ref ever has one.
        if raw.starts_with('-') {
            return Err(RefNameError::LeadingDash);
        }
        if raw == Self::RESERVED {
            return Err(RefNameError::Reserved(Self::RESERVED));
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
    ///
    /// A struct literal is equally valid and clearer when only a few
    /// fields differ; this helper exists for all-field construction.
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

    fn bad_path(raw: &str) -> Option<&'static str> {
        match RefName::parse(raw) {
            Err(RefNameError::BadPath(rule)) => Some(rule),
            _ => None,
        }
    }

    #[test]
    fn accepts_path_like_names() {
        for raw in [
            "main",
            "feature/nightly",
            "a",
            "x-y.z_w",
            "x-",
            "a/-b",
            "v1.2",
            "1/2",
            "123",
            "head",
            "Head",
            "HEAD/x",
            "x/HEAD",
            "a.b/c.d",
            "x.lock",
            "x.lock/y",
            ".hidden",
            "a/.b",
            "a/./b",
            "a./b",
        ] {
            assert_eq!(RefName::parse(raw).unwrap().as_str(), raw);
        }
        assert_eq!(RefName::from_str("main").unwrap().to_string(), "main");
    }

    #[test]
    fn rejects_bad_characters() {
        assert_eq!(RefName::parse(""), Err(RefNameError::Empty));
        assert_eq!(RefName::parse("a b"), Err(RefNameError::BadChar(' ')));
        assert_eq!(RefName::parse("@x"), Err(RefNameError::BadChar('@')));
        // A multi-byte character is reported as itself, not as one of its
        // bytes.
        assert_eq!(RefName::parse("bré"), Err(RefNameError::BadChar('é')));
    }

    #[test]
    fn rejects_empty_components_and_double_dots() {
        for raw in ["/main", "main/", "a//b"] {
            assert_eq!(bad_path(raw), Some("empty path component"), "{raw}");
        }
        for raw in ["..", "v..1", "a/../b", "a/..", "x..lock"] {
            assert_eq!(bad_path(raw), Some("contains '..'"), "{raw}");
        }
        for raw in [".", "end.", "a/b."] {
            assert_eq!(bad_path(raw), Some("ends with '.'"), "{raw}");
        }
    }

    #[test]
    fn rejects_a_leading_dash() {
        for raw in ["-", "-x", "-x/y"] {
            assert_eq!(RefName::parse(raw), Err(RefNameError::LeadingDash), "{raw}");
        }
    }

    #[test]
    fn reserves_only_the_exact_name_head() {
        assert_eq!(
            RefName::parse("HEAD"),
            Err(RefNameError::Reserved(RefName::RESERVED))
        );
    }

    #[test]
    fn limits_the_whole_name_to_255_bytes() {
        assert!(RefName::parse(&"a".repeat(RefName::MAX_LEN)).is_ok());
        assert_eq!(
            RefName::parse(&"a".repeat(RefName::MAX_LEN + 1)),
            Err(RefNameError::TooLong(RefName::MAX_LEN))
        );
        let component = "a".repeat(100);
        assert_eq!(
            RefName::parse(&[component.as_str(); 3].join("/")),
            Err(RefNameError::TooLong(RefName::MAX_LEN)),
            "the limit covers the whole name, not each component"
        );
        // The byte-length check runs first: an over-long name reports
        // `TooLong` even when it also holds a non-ASCII character.
        assert_eq!(
            RefName::parse(&"é".repeat(RefName::MAX_LEN)),
            Err(RefNameError::TooLong(RefName::MAX_LEN))
        );
    }

    #[test]
    fn unborn_refs_have_no_commit() {
        let name = RefName::parse("main").unwrap();
        let record = CommitRef::new(name.clone(), None, true, 1_700_000_000_000);
        assert!(record.is_unborn());
        assert!(record.active);
        assert_eq!(record.name, name);
    }

    #[test]
    fn born_refs_name_a_commit() {
        let name = RefName::parse("main").unwrap();
        let commit = CommitId([7; 32]);
        let record = CommitRef::new(name, Some(commit), false, 1_700_000_000_000);
        assert!(!record.is_unborn());
        assert!(!record.active);
        assert_eq!(record.commit, Some(commit));
    }
}
