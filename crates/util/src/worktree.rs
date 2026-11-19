//! Worktrees: the worlds a store backs up, each with its own HEAD
//! (ADR-0035).

use alloc::string::{String, ToString};
use core::{fmt, str::FromStr};

use crate::commit::CommitId;
use crate::error::WorktreeNameError;
use crate::name;
use crate::refs::RefName;

/// Validated worktree name.
///
/// A name is one ref component: 1-255 bytes of ASCII alphanumerics plus
/// `.`, `_`, and `-`, with no `..`. Git's worktree IDs are single
/// components too, sanitized from the basename of the worktree's path.
/// Unlike a ref name, `HEAD` is not reserved: a worktree name never reads
/// as a revision.
///
/// Ordering and uniqueness are byte-wise and case-sensitive.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorktreeName(String);

impl WorktreeName {
    /// Longest accepted name, in bytes (names are ASCII-only).
    pub const MAX_LEN: usize = name::MAX_LEN;

    /// Validate `raw` into a worktree name.
    ///
    /// The byte-length check runs first, so an over-long name reports
    /// [`WorktreeNameError::TooLong`] even when it also holds a non-ASCII
    /// character.
    pub fn parse(raw: &str) -> Result<Self, WorktreeNameError> {
        name::check(raw).map_err(WorktreeNameError::from)?;
        if raw.contains('/') {
            return Err(WorktreeNameError::BadChar('/'));
        }
        if raw.contains("..") {
            return Err(WorktreeNameError::DoubleDot);
        }
        Ok(Self(String::from(raw)))
    }

    /// Sanitize the basename of a world's path into a worktree name, as
    /// Git 2.53 sanitizes worktree IDs: `..` collapses to `.`, a leading
    /// `.` becomes `-`, and a basename ending in `.lock` loses every
    /// trailing `.lock`. So `a..b` becomes `a.b` and `.x` becomes `-x`.
    ///
    /// Git keeps every character a ref component allows, non-ASCII ones
    /// included. sekai's names are narrower, so each character outside
    /// them becomes `-`, as Git does for the characters it refuses
    /// **(deviation)**. A name over [`Self::MAX_LEN`] bytes is cut to fit.
    ///
    /// Fails only for an empty basename.
    pub fn sanitize(basename: &str) -> Result<Self, WorktreeNameError> {
        let mut out = String::with_capacity(basename.len());
        let mut last = None;
        for ch in basename.chars() {
            if !(ch == '.' && last == Some('.')) {
                out.push(if name::is_component_char(ch) { ch } else { '-' });
            }
            last = Some(ch);
        }
        if basename.starts_with('.') {
            out.replace_range(..1, "-");
        }
        // Case-sensitive, as Git compares it: `x.LOCK` keeps its suffix.
        if basename.as_bytes().ends_with(b".lock") {
            while let Some(stripped) = out.strip_suffix(".lock") {
                out.truncate(stripped.len());
            }
        }
        // Every character is ASCII by now, so any byte is a boundary.
        out.truncate(Self::MAX_LEN);
        Self::parse(&out)
    }

    /// This name with the numeric suffix Git appends on a clash: `foo`
    /// becomes `foo1`, `foo2`, and so on. The name is cut first when the
    /// suffix would push it over [`Self::MAX_LEN`] bytes. A cut can leave
    /// a trailing `.` behind, which `parse` accepts; it can never leave
    /// `..` behind, and the digits never add one, so the result is
    /// always a name `parse` would accept.
    #[must_use]
    pub fn with_suffix(&self, n: u32) -> Self {
        let digits = n.checked_ilog10().map_or(1, |log| log as usize + 1);
        let mut name =
            String::from(&self.0[..self.0.len().min(Self::MAX_LEN.saturating_sub(digits))]);
        // Cutting cannot leave `..` behind, and a digit never adds one.
        name.push_str(&n.to_string());
        Self(name)
    }

    /// Borrow the validated name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorktreeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for WorktreeName {
    type Err = WorktreeNameError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

/// What a worktree's HEAD stores: a branch name or a commit.
///
/// Whether a branch HEAD is born depends on whether the branch exists,
/// which only the store knows; [`HeadState`] carries that answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    /// HEAD names a branch (`refs/heads/<name>`), which may be unborn.
    Branch(RefName),
    /// HEAD holds a commit directly.
    Detached(CommitId),
}

impl Head {
    /// The branch HEAD names, or `None` when HEAD is detached.
    pub const fn branch(&self) -> Option<&RefName> {
        match self {
            Self::Branch(name) => Some(name),
            Self::Detached(_) => None,
        }
    }

    /// Resolve this HEAD against the store. `branch_commit` is the
    /// commit the named branch points at, or `None` while the branch
    /// does not exist; a detached HEAD ignores it.
    pub fn resolve(&self, branch_commit: Option<CommitId>) -> HeadState {
        match self {
            Self::Branch(branch) => branch_commit.map_or_else(
                || HeadState::Unborn {
                    branch: branch.clone(),
                },
                |commit| HeadState::Attached {
                    branch: branch.clone(),
                    commit,
                },
            ),
            Self::Detached(commit) => HeadState::Detached { commit: *commit },
        }
    }
}

/// A worktree's HEAD resolved against the store's refs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadState {
    /// HEAD names an existing branch.
    Attached {
        /// Branch HEAD names.
        branch: RefName,
        /// Commit the branch points at.
        commit: CommitId,
    },
    /// HEAD names a branch that has no commit yet.
    Unborn {
        /// Branch the next commit creates.
        branch: RefName,
    },
    /// HEAD holds a commit directly.
    Detached {
        /// Commit HEAD holds.
        commit: CommitId,
    },
}

impl HeadState {
    /// The commit HEAD stands at, or `None` while it is unborn.
    pub const fn commit(&self) -> Option<CommitId> {
        match self {
            Self::Attached { commit, .. } | Self::Detached { commit } => Some(*commit),
            Self::Unborn { .. } => None,
        }
    }

    /// The branch HEAD names, or `None` when HEAD is detached.
    pub const fn branch(&self) -> Option<&RefName> {
        match self {
            Self::Attached { branch, .. } | Self::Unborn { branch } => Some(branch),
            Self::Detached { .. } => None,
        }
    }
}

/// One world registered in a store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// Worktree name.
    pub name: WorktreeName,
    /// Absolute world root of a linked worktree, or `None` for the main
    /// worktree, whose world root is the directory holding the store.
    pub path: Option<String>,
    /// What HEAD stores.
    pub head: Head,
}

impl Worktree {
    /// Whether this is the main worktree: the world that holds the store.
    pub const fn is_main(&self) -> bool {
        self.path.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn accepts_single_components() {
        for raw in ["main", "world-2", "x.y_z", "HEAD", ".hidden", "end.", "1"] {
            assert_eq!(WorktreeName::parse(raw).unwrap().as_str(), raw);
        }
        assert_eq!(WorktreeName::from_str("w").unwrap().to_string(), "w");
    }

    #[test]
    fn rejects_paths_double_dots_and_bad_characters() {
        assert_eq!(WorktreeName::parse(""), Err(WorktreeNameError::Empty));
        for raw in ["a/b", "/a", "a/"] {
            assert_eq!(
                WorktreeName::parse(raw),
                Err(WorktreeNameError::BadChar('/')),
                "{raw}"
            );
        }
        for raw in ["..", "a..b"] {
            assert_eq!(
                WorktreeName::parse(raw),
                Err(WorktreeNameError::DoubleDot),
                "{raw}"
            );
        }
        assert_eq!(
            WorktreeName::parse("a b"),
            Err(WorktreeNameError::BadChar(' '))
        );
        assert_eq!(
            WorktreeName::parse(&"a".repeat(WorktreeName::MAX_LEN + 1)),
            Err(WorktreeNameError::TooLong(WorktreeName::MAX_LEN))
        );
        assert!(WorktreeName::parse(&"a".repeat(WorktreeName::MAX_LEN)).is_ok());
    }

    #[test]
    fn sanitizes_basenames_as_git_does() {
        // Expected names come from `git worktree add --detach <dir>/<basename>`
        // on git 2.53, except where sekai's narrower character set differs.
        for (basename, expected) in [
            ("has space", "has-space"),
            ("a..b", "a.b"),
            ("a...b", "a.b"),
            ("x:y", "x-y"),
            ("a~b^c", "a-b-c"),
            (".lead", "-lead"),
            ("..x", "-x"),
            (".a.", "-a."),
            ("...", "-"),
            ("a .b", "a-.b"),
            ("x.", "x."),
            ("-x", "-x"),
            ("trail.lock", "trail"),
            ("a.lock.lock", "a"),
            ("x..lock", "x"),
            (".lock", "-lock"),
            ("a.lock.b", "a.lock.b"),
            ("x.lock.", "x.lock."),
            ("x.LOCK", "x.LOCK"),
            // Git keeps these characters; sekai's names cannot hold them.
            ("a@{b", "a--b"),
            ("ab+c=d", "ab-c-d"),
            ("\u{e9}", "-"),
            ("\u{e9}.lock", "-"),
        ] {
            assert_eq!(
                WorktreeName::sanitize(basename).unwrap().as_str(),
                expected,
                "{basename:?}"
            );
        }
        assert_eq!(WorktreeName::sanitize(""), Err(WorktreeNameError::Empty));
        let long = "a".repeat(WorktreeName::MAX_LEN + 10);
        assert_eq!(
            WorktreeName::sanitize(&long).unwrap().as_str().len(),
            WorktreeName::MAX_LEN
        );
    }

    #[test]
    fn suffixes_count_from_one_and_fit_the_limit() {
        let foo = WorktreeName::parse("foo").unwrap();
        assert_eq!(foo.with_suffix(1).as_str(), "foo1");
        assert_eq!(foo.with_suffix(12).as_str(), "foo12");
        assert_eq!(
            WorktreeName::parse("foo1").unwrap().with_suffix(1).as_str(),
            "foo11"
        );
        let full = WorktreeName::parse(&"a".repeat(WorktreeName::MAX_LEN)).unwrap();
        let suffixed = full.with_suffix(100);
        assert_eq!(suffixed.as_str().len(), WorktreeName::MAX_LEN);
        assert!(suffixed.as_str().ends_with("a100"));
        assert_eq!(WorktreeName::parse(suffixed.as_str()), Ok(suffixed));
    }

    #[test]
    fn head_states_expose_their_commit_and_branch() {
        let branch = RefName::parse("main").unwrap();
        let commit = CommitId([3; 32]);
        let attached = HeadState::Attached {
            branch: branch.clone(),
            commit,
        };
        let unborn = HeadState::Unborn {
            branch: branch.clone(),
        };
        let detached = HeadState::Detached { commit };
        assert_eq!(attached.commit(), Some(commit));
        assert_eq!(unborn.commit(), None);
        assert_eq!(detached.commit(), Some(commit));
        assert_eq!(attached.branch(), Some(&branch));
        assert_eq!(unborn.branch(), Some(&branch));
        assert_eq!(detached.branch(), None);
        assert_eq!(Head::Branch(branch.clone()).branch(), Some(&branch));
        assert_eq!(Head::Detached(commit).branch(), None);
    }

    #[test]
    fn head_resolves_against_the_branch_commit() {
        let branch = RefName::parse("main").unwrap();
        let commit = CommitId([3; 32]);
        let named = Head::Branch(branch.clone());
        assert_eq!(
            named.resolve(Some(commit)),
            HeadState::Attached {
                branch: branch.clone(),
                commit,
            }
        );
        assert_eq!(named.resolve(None), HeadState::Unborn { branch });
        assert_eq!(
            Head::Detached(commit).resolve(None),
            HeadState::Detached { commit }
        );
    }
}
