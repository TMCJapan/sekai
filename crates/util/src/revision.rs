//! Revision syntax: the subset of gitrevisions sekai adopts.
//!
//! This module checks the grammar only, so the command line can refuse a
//! malformed revision at parse time (ADR-0013). What a revision names is
//! decided against a store at run time.
//!
//! Ref names hold only ASCII alphanumerics, `.`, `_`, `-`, and `/`, and
//! are never spelled like a commit ID, so `@`, `~`, and a 64-digit hex
//! spelling always belong to the revision syntax.

use alloc::string::String;
use core::{fmt, str::FromStr};

use crate::commit::CommitId;
use crate::error::RevisionError;
use crate::refs::RefName;

/// A commit-ish argument: a starting point, optionally followed by one
/// `~<n>` step to its `n`-th first-parent ancestor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision {
    text: String,
    base: RevisionBase,
    ancestor: u32,
}

/// Where a revision starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevisionBase {
    /// `HEAD`, or its alias `@`.
    Head,
    /// A 64-digit hex commit ID.
    Commit(CommitId),
    /// A ref name. [`RefName`] refuses commit-ID spellings, so a name
    /// never doubles as a commit ID.
    Name(RefName),
}

impl Revision {
    /// Check `text` against the revision grammar.
    pub fn parse(text: &str) -> Result<Self, RevisionError> {
        let (base, ancestor) = match text.split_once('~') {
            Some((base, digits)) => (base, Some(digits)),
            None => (text, None),
        };
        Ok(Self {
            text: String::from(text),
            base: parse_base(base)?,
            ancestor: ancestor.map_or(Ok(0), parse_ancestor)?,
        })
    }

    /// `HEAD`, which commands read when no revision is given.
    pub fn head() -> Self {
        Self {
            text: String::from("HEAD"),
            base: RevisionBase::Head,
            ancestor: 0,
        }
    }

    /// The revision as written.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Where the revision starts.
    pub const fn base(&self) -> &RevisionBase {
        &self.base
    }

    /// How many first parents the `~<n>` step follows from the base; 0
    /// without one.
    pub const fn ancestor(&self) -> u32 {
        self.ancestor
    }
}

fn parse_base(base: &str) -> Result<RevisionBase, RevisionError> {
    if base.is_empty() {
        return Err(RevisionError::Empty);
    }
    if base == "HEAD" || base == "@" {
        return Ok(RevisionBase::Head);
    }
    // `@` never appears in a ref name, so whatever follows a leading `@`
    // is a step sekai does not adopt (`@^`, `@{1}`).
    if base.starts_with('@') {
        return Err(RevisionError::BadStep);
    }
    if let Ok(id) = base.parse::<CommitId>() {
        return Ok(RevisionBase::Commit(id));
    }
    RefName::parse(base)
        .map(RevisionBase::Name)
        .map_err(RevisionError::Name)
}

/// Parse what follows the `~`; `~` alone is `~1`.
///
/// Only one step is accepted. Git chains them (`HEAD~~` is `HEAD~2`), but
/// without `^` a second step adds nothing over a larger `n`, so the count
/// is written once (deviation).
fn parse_ancestor(digits: &str) -> Result<u32, RevisionError> {
    if digits.is_empty() {
        return Ok(1);
    }
    // `u32::from_str` takes a leading `+`, which is not a step.
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(RevisionError::BadStep);
    }
    digits.parse().map_err(|_| RevisionError::BadStep)
}

impl fmt::Display for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl FromStr for Revision {
    type Err = RevisionError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::RefNameError;

    fn name(raw: &str) -> RefName {
        RefName::parse(raw).unwrap()
    }

    fn parsed(text: &str) -> (RevisionBase, u32) {
        let revision = Revision::parse(text).unwrap();
        assert_eq!(revision.as_str(), text);
        (revision.base().clone(), revision.ancestor())
    }

    #[test]
    fn parses_bases() {
        assert_eq!(parsed("HEAD"), (RevisionBase::Head, 0));
        assert_eq!(parsed("@"), (RevisionBase::Head, 0));
        assert_eq!(Revision::head(), Revision::parse("HEAD").unwrap());
        assert_eq!(parsed("main"), (RevisionBase::Name(name("main")), 0));
        assert_eq!(parsed("HEAD/x"), (RevisionBase::Name(name("HEAD/x")), 0));
        let hex = "a".repeat(64);
        let id = CommitId::from_hex(hex.as_bytes()).unwrap();
        assert_eq!(parsed(&hex), (RevisionBase::Commit(id), 0));
    }

    #[test]
    fn parses_steps() {
        assert_eq!(parsed("HEAD~"), (RevisionBase::Head, 1));
        assert_eq!(parsed("@~3"), (RevisionBase::Head, 3));
        assert_eq!(parsed("HEAD~0"), (RevisionBase::Head, 0));
        assert_eq!(parsed("main~12"), (RevisionBase::Name(name("main")), 12));
        // `~` is cut before the name is checked, so `a~1` is never a name.
        assert_eq!(parsed("a~1"), (RevisionBase::Name(name("a")), 1));
    }

    #[test]
    fn refuses_what_is_not_adopted() {
        for (text, err) in [
            ("", RevisionError::Empty),
            ("~1", RevisionError::Empty),
            ("^", RevisionError::Name(RefNameError::BadChar('^'))),
            ("@^", RevisionError::BadStep),
            ("@{1}", RevisionError::BadStep),
            ("main^", RevisionError::Name(RefNameError::BadChar('^'))),
            ("main~1^2", RevisionError::BadStep),
            ("main~1~2", RevisionError::BadStep),
            ("HEAD~~", RevisionError::BadStep),
            ("main~+1", RevisionError::BadStep),
            ("main@{1}", RevisionError::Name(RefNameError::BadChar('@'))),
            ("a b", RevisionError::Name(RefNameError::BadChar(' '))),
            ("main~x", RevisionError::BadStep),
            ("main~99999999999", RevisionError::BadStep),
            (":/text", RevisionError::Name(RefNameError::BadChar(':'))),
            ("main:path", RevisionError::Name(RefNameError::BadChar(':'))),
            (
                "main..dev",
                RevisionError::Name(RefNameError::BadPath("contains '..'")),
            ),
        ] {
            assert_eq!(Revision::parse(text), Err(err), "{text:?}");
        }
    }
}
