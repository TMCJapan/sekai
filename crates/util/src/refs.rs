//! Named pointers to commits, modelled on Git's refs (ADR-0035).
//!
//! A store holds branches (`refs/heads/<name>`), which move as commits
//! land on them, and lightweight tags (`refs/tags/<name>`). Every stored
//! ref names a commit: a branch without one is *unborn* and exists only as
//! the name a worktree's HEAD carries.

use alloc::string::String;
use core::{fmt, str::FromStr};

use crate::commit::CommitId;
use crate::error::RefNameError;
use crate::name;

/// Branch a new worktree's HEAD starts on, unborn until its first commit.
pub const DEFAULT_REF_NAME: &str = "main";

/// Validated short ref name: the part after `refs/heads/` or `refs/tags/`.
///
/// Names are path-like (`main`, `feature/nightly`): 1-255 bytes of ASCII
/// alphanumerics plus `.`, `_`, `-`, and `/`, with no empty `/`-separated
/// component, no `..`, no trailing `.`, and no leading `-` (ADR-0035).
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

/// Namespace a ref lives in.
///
/// `Branch` is declared before `Tag` so that the derived order matches
/// the byte order of the full refnames (`refs/heads/` < `refs/tags/`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RefKind {
    /// `refs/heads/<name>`.
    Branch,
    /// `refs/tags/<name>`, a lightweight tag.
    Tag,
}

impl RefKind {
    /// Every kind, in refname order.
    pub const ALL: [Self; 2] = [Self::Branch, Self::Tag];

    /// Refname prefix of this namespace, trailing `/` included.
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Branch => "refs/heads/",
            Self::Tag => "refs/tags/",
        }
    }

    /// The full Git refname of `name` in this namespace, such as
    /// `refs/heads/main`.
    pub fn full_name(self, name: &RefName) -> String {
        let mut full = String::from(self.prefix());
        full.push_str(name.as_str());
        full
    }
}

/// A branch or tag, identified by its namespace and short name.
///
/// The store keys refs by the full Git refname (`refs/heads/main`), which
/// [`Self::full_name`] and [`Self::from_full`] convert to and from. The
/// derived order equals the byte order of the full refnames.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ref {
    /// Namespace.
    pub kind: RefKind,
    /// Short name within the namespace.
    pub name: RefName,
}

impl Ref {
    /// Construct a ref from its parts.
    pub const fn new(kind: RefKind, name: RefName) -> Self {
        Self { kind, name }
    }

    /// The branch `refs/heads/<name>`.
    pub const fn branch(name: RefName) -> Self {
        Self::new(RefKind::Branch, name)
    }

    /// The tag `refs/tags/<name>`.
    pub const fn tag(name: RefName) -> Self {
        Self::new(RefKind::Tag, name)
    }

    /// The full Git refname, such as `refs/heads/main`.
    pub fn full_name(&self) -> String {
        self.kind.full_name(&self.name)
    }

    /// Parse a full refname. Anything outside `refs/heads/` and
    /// `refs/tags/` fails with [`RefNameError::UnknownNamespace`].
    pub fn from_full(raw: &str) -> Result<Self, RefNameError> {
        let (kind, short) = RefKind::ALL
            .into_iter()
            .find_map(|kind| raw.strip_prefix(kind.prefix()).map(|short| (kind, short)))
            .ok_or(RefNameError::UnknownNamespace)?;
        Ok(Self::new(kind, RefName::parse(short)?))
    }
}

impl fmt::Display for Ref {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.kind.prefix(), self.name)
    }
}

/// A stored ref and the commit it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRef {
    /// Ref.
    pub name: Ref,
    /// Commit the ref points at.
    pub commit: CommitId,
}

impl CommitRef {
    /// Construct a ref record.
    pub const fn new(name: Ref, commit: CommitId) -> Self {
        Self { name, commit }
    }
}

/// Whether two full refnames conflict as a directory and a file would:
/// one is a `/`-separated prefix of the other.
///
/// The store keys refs by full name, so `refs/heads/a` and `refs/heads/a/b`
/// cannot coexist any more than a directory and a file can. Names in
/// different namespaces never conflict.
pub fn paths_conflict(a: &str, b: &str) -> bool {
    fn nested(outer: &str, inner: &str) -> bool {
        inner
            .strip_prefix(outer)
            .is_some_and(|rest| rest.starts_with('/'))
    }
    nested(a, b) || nested(b, a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec::Vec;

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
    fn full_names_round_trip() {
        for (full, kind, short) in [
            ("refs/heads/main", RefKind::Branch, "main"),
            ("refs/heads/feature/x", RefKind::Branch, "feature/x"),
            ("refs/tags/v1", RefKind::Tag, "v1"),
        ] {
            let r = Ref::from_full(full).unwrap();
            assert_eq!(r, Ref::new(kind, RefName::parse(short).unwrap()));
            assert_eq!(r.full_name(), full);
            assert_eq!(r.to_string(), full);
        }
    }

    #[test]
    fn full_names_outside_heads_and_tags_are_refused() {
        for raw in [
            "main",
            "heads/main",
            "refs/main",
            "refs/remotes/o/main",
            "refs/heads",
        ] {
            assert_eq!(
                Ref::from_full(raw),
                Err(RefNameError::UnknownNamespace),
                "{raw}"
            );
        }
        assert_eq!(Ref::from_full("refs/heads/"), Err(RefNameError::Empty));
        assert_eq!(
            Ref::from_full("refs/tags/HEAD"),
            Err(RefNameError::Reserved(RefName::RESERVED))
        );
    }

    #[test]
    fn paths_conflict_tests() {
        for (a, b, expected) in [
            ("refs/heads/a", "refs/heads/a/b", true),
            ("refs/heads/a/b", "refs/heads/a", true),
            ("refs/heads/a", "refs/heads/a", false),
            ("refs/heads/a", "refs/heads/a-b", false),
            ("refs/heads/a", "refs/heads/ab", false),
            ("refs/heads/a", "refs/tags/a", false),
            ("refs/heads/a", "refs/tags/a/b", false),
            ("refs/heads/feature/x", "refs/heads/feature", true),
            ("refs/heads/feature", "refs/heads/feature/x", true),
            ("refs/heads/feature/x/y", "refs/heads/feature", true),
            ("refs/heads/feature/x", "refs/heads/feature/y", false),
        ] {
            assert_eq!(paths_conflict(a, b), expected, "{a} vs {b}");
            assert_eq!(paths_conflict(b, a), expected, "{b} vs {a}");
        }
    }

    #[test]
    fn ref_order_matches_full_name_order() {
        let name = |raw| RefName::parse(raw).unwrap();
        let mut refs = alloc::vec![
            Ref::new(RefKind::Tag, name("a")),
            Ref::new(RefKind::Branch, name("z")),
            Ref::new(RefKind::Branch, name("a/b")),
            Ref::new(RefKind::Branch, name("a-b")),
            Ref::new(RefKind::Tag, name("A")),
        ];
        let mut full: Vec<String> = refs.iter().map(Ref::full_name).collect();
        refs.sort();
        full.sort();
        assert_eq!(refs.iter().map(Ref::full_name).collect::<Vec<_>>(), full);
    }
}
