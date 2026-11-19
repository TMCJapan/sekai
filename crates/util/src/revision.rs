//! Revision syntax, the subset of gitrevisions that ADR-0036 adopts.
//!
//! This module checks the grammar only, so the command line can refuse a
//! malformed revision at parse time (ADR-0013). What a revision names is
//! decided against a store at run time.
//!
//! sekai's ref names hold only ASCII alphanumerics, `.`, `_`, `-`, and
//! `/`, never `..`, and never end in `.`, so `@`, `~`, and `..` always
//! belong to the revision syntax.

use alloc::string::String;
use alloc::vec::Vec;
use core::{fmt, str::FromStr};

use crate::error::RevisionError;
use crate::refs::RefName;

/// A commit-ish argument: a starting point followed by parent steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision {
    text: String,
    base: RevisionBase,
    steps: Vec<RevisionStep>,
}

/// Where a revision starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevisionBase {
    /// `HEAD`, or its alias `@`.
    Head,
    /// A commit ID or a ref name. Both spellings fit [`RefName`], and a
    /// store tells them apart (ADR-0035 *Resolution*).
    Name(RefName),
}

/// The reflog a `reflog` command reads (ADR-0036 *reflog*).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReflogSelector {
    /// HEAD's log.
    Head,
    /// The log of the branch `<ref>` resolves to.
    Ref(RefName),
}

/// One step from a commit to an ancestor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionStep {
    /// `~<n>`: the `n`-th first-parent ancestor. `~` alone is `~1`.
    Ancestor(u32),
}

impl Revision {
    /// Check `text` against the revision grammar.
    pub fn parse(text: &str) -> Result<Self, RevisionError> {
        if text.is_empty() {
            return Err(RevisionError::Empty);
        }
        let split = text.find('~').unwrap_or(text.len());
        let (base, steps) = text.split_at(split);
        let base = parse_base(base)?;
        let steps = parse_steps(steps)?;
        Ok(Self {
            text: String::from(text),
            base,
            steps,
        })
    }

    /// `HEAD`, which commands read when no revision is given.
    pub fn head() -> Self {
        Self {
            text: String::from("HEAD"),
            base: RevisionBase::Head,
            steps: Vec::new(),
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

    /// Steps from the base to an ancestor, in the order written.
    pub fn steps(&self) -> &[RevisionStep] {
        &self.steps
    }
}

fn parse_base(base: &str) -> Result<RevisionBase, RevisionError> {
    if base.is_empty() {
        return Err(RevisionError::Empty);
    }
    if base == "HEAD" || base == "@" {
        return Ok(RevisionBase::Head);
    }
    RefName::parse(base)
        .map(RevisionBase::Name)
        .map_err(RevisionError::Name)
}

fn parse_steps(mut rest: &str) -> Result<Vec<RevisionStep>, RevisionError> {
    let mut steps = Vec::new();
    while let Some(op) = rest.chars().next() {
        let tail = &rest[op.len_utf8()..];
        let digits = tail
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(tail.len());
        let n = match &tail[..digits] {
            "" => 1,
            digits => parse_number(digits).ok_or(RevisionError::BadStep)?,
        };
        if op != '~' {
            return Err(RevisionError::BadStep);
        }
        steps.push(RevisionStep::Ancestor(n));
        rest = &tail[digits..];
    }
    Ok(steps)
}

/// A decimal count; `None` on overflow.
fn parse_number(digits: &str) -> Option<u32> {
    digits.parse().ok()
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

/// A `log` argument: a revision to start from, one to exclude, or a
/// range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevisionRange {
    /// `<rev>`: commits reachable from the revision.
    Include(Revision),
    /// `^<rev>`: leave out commits reachable from the revision.
    Exclude(Revision),
    /// `<a>..<b>`: commits reachable from `b` but not from `a`. An
    /// omitted side is `HEAD`, as in Git, but `..` alone is refused.
    Between {
        /// Excluded side.
        from: Revision,
        /// Included side.
        to: Revision,
    },
}

impl RevisionRange {
    /// Check `text` against the range grammar.
    pub fn parse(text: &str) -> Result<Self, RevisionError> {
        if let Some(excluded) = text.strip_prefix('^') {
            if excluded.starts_with(['^', '~']) {
                // A doubled prefix, such as `^^main`: the revision
                // syntax opens with one step operator, not two.
                return Err(RevisionError::BadStep);
            }
            return Revision::parse(excluded).map(Self::Exclude);
        }
        let Some((from, to)) = text.split_once("..") else {
            return Revision::parse(text).map(Self::Include);
        };
        // A ref name never holds `..`, so the first `..` ends the left
        // side. A single `.` right after it spells the unsupported
        // symmetric difference; a longer run falls through to the
        // revision parser, which names the bad name itself.
        if to.starts_with('.') && !to.starts_with("..") {
            return Err(RevisionError::SymmetricDifference);
        }
        if from.is_empty() && to.is_empty() {
            return Err(RevisionError::Empty);
        }
        let side = |side: &str| {
            if side.is_empty() {
                Ok(Revision::head())
            } else {
                Revision::parse(side)
            }
        };
        Ok(Self::Between {
            from: side(from)?,
            to: side(to)?,
        })
    }
}

/// The side of a `log` walk a revision falls on: commits reachable from
/// an included revision are listed, and commits reachable from an
/// excluded one are subtracted (gitrevisions *Specifying ranges*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side<T> {
    /// `<rev>`, or the right side of `<a>..<b>`.
    Include(T),
    /// `^<rev>`, or the left side of `<a>..<b>`.
    Exclude(T),
}

impl RevisionRange {
    /// The revisions this argument names, each on its side, in the order
    /// Git resolves them: `<a>..<b>` is `^<a> <b>`, as Git's
    /// `handle_dotdot` adds them.
    pub fn sides(&self) -> impl Iterator<Item = Side<&Revision>> {
        let (first, second) = match self {
            Self::Include(revision) => (Side::Include(revision), None),
            Self::Exclude(revision) => (Side::Exclude(revision), None),
            Self::Between { from, to } => (Side::Exclude(from), Some(Side::Include(to))),
        };
        core::iter::once(first).chain(second)
    }
}

impl FromStr for RevisionRange {
    type Err = RevisionError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::RefNameError;
    use alloc::vec;

    fn name(raw: &str) -> RefName {
        RefName::parse(raw).unwrap()
    }

    fn parsed(text: &str) -> (RevisionBase, Vec<RevisionStep>) {
        let revision = Revision::parse(text).unwrap();
        assert_eq!(revision.as_str(), text);
        (revision.base().clone(), revision.steps().to_vec())
    }

    #[test]
    fn parses_bases() {
        assert_eq!(parsed("HEAD"), (RevisionBase::Head, vec![]));
        assert_eq!(parsed("@"), (RevisionBase::Head, vec![]));
        assert_eq!(Revision::head(), Revision::parse("HEAD").unwrap());
        assert_eq!(parsed("main"), (RevisionBase::Name(name("main")), vec![]));
        assert_eq!(
            parsed("refs/heads/x"),
            (RevisionBase::Name(name("refs/heads/x")), vec![])
        );
        let hex = "a".repeat(64);
        assert_eq!(parsed(&hex), (RevisionBase::Name(name(&hex)), vec![]));
        assert_eq!(
            parsed("HEAD/x"),
            (RevisionBase::Name(name("HEAD/x")), vec![])
        );
    }

    #[test]
    fn parses_steps() {
        use RevisionStep::Ancestor;
        assert_eq!(parsed("HEAD~"), (RevisionBase::Head, vec![Ancestor(1)]));
        assert_eq!(parsed("@~3"), (RevisionBase::Head, vec![Ancestor(3)]));
        assert_eq!(
            parsed("main~1~2"),
            (
                RevisionBase::Name(name("main")),
                vec![Ancestor(1), Ancestor(2)]
            )
        );
    }

    #[test]
    fn refuses_what_adr_0036_leaves_out() {
        for (text, err) in [
            ("", RevisionError::Empty),
            ("~1", RevisionError::Empty),
            ("^", RevisionError::Name(RefNameError::BadChar('^'))),
            ("@^", RevisionError::Name(RefNameError::BadChar('@'))),
            ("main^", RevisionError::Name(RefNameError::BadChar('^'))),
            ("main~1^2", RevisionError::BadStep),
            ("main@{1}", RevisionError::Name(RefNameError::BadChar('@'))),
            (
                "main@{upstream}",
                RevisionError::Name(RefNameError::BadChar('@')),
            ),
            ("a b", RevisionError::Name(RefNameError::BadChar(' '))),
            (
                "main^{commit}",
                RevisionError::Name(RefNameError::BadChar('^')),
            ),
            ("main~x", RevisionError::BadStep),
            ("main~99999999999", RevisionError::BadStep),
            (":/text", RevisionError::Name(RefNameError::BadChar(':'))),
            ("main:path", RevisionError::Name(RefNameError::BadChar(':'))),
        ] {
            assert_eq!(Revision::parse(text), Err(err), "{text:?}");
        }
    }

    #[test]
    fn parses_ranges() {
        let rev = |text| Revision::parse(text).unwrap();
        assert_eq!(
            RevisionRange::parse("main"),
            Ok(RevisionRange::Include(rev("main")))
        );
        assert_eq!(
            RevisionRange::parse("^main~2"),
            Ok(RevisionRange::Exclude(rev("main~2")))
        );
        assert_eq!(
            RevisionRange::parse("a..b"),
            Ok(RevisionRange::Between {
                from: rev("a"),
                to: rev("b")
            })
        );
        assert_eq!(
            RevisionRange::parse("HEAD~1.."),
            Ok(RevisionRange::Between {
                from: rev("HEAD~1"),
                to: rev("HEAD")
            })
        );
        assert_eq!(
            RevisionRange::parse("..b"),
            Ok(RevisionRange::Between {
                from: rev("HEAD"),
                to: rev("b")
            })
        );
        assert_eq!(RevisionRange::parse(".."), Err(RevisionError::Empty));
        assert_eq!(
            RevisionRange::parse("a...b"),
            Err(RevisionError::SymmetricDifference)
        );
        assert_eq!(
            RevisionRange::parse("a..b..c"),
            Err(RevisionError::Name(RefNameError::BadPath("contains '..'")))
        );
        assert_eq!(
            RevisionRange::parse("main..HEAD."),
            Err(RevisionError::Name(RefNameError::BadPath("ends with '.'")))
        );
        // A doubled prefix is a step-syntax slip, not an empty revision.
        assert_eq!(RevisionRange::parse("^^main"), Err(RevisionError::BadStep));
        assert_eq!(RevisionRange::parse("^~1"), Err(RevisionError::BadStep));
    }

    #[test]
    fn a_range_opens_into_sides() {
        fn sides(range: &RevisionRange) -> Vec<Side<&Revision>> {
            range.sides().collect()
        }
        let rev = |text| Revision::parse(text).unwrap();
        let range = |text| RevisionRange::parse(text).unwrap();
        assert_eq!(sides(&range("main")), [Side::Include(&rev("main"))]);
        assert_eq!(sides(&range("^main")), [Side::Exclude(&rev("main"))]);
        assert_eq!(
            sides(&range("a..b")),
            [Side::Exclude(&rev("a")), Side::Include(&rev("b"))]
        );
        assert_eq!(
            sides(&range("a..")),
            [Side::Exclude(&rev("a")), Side::Include(&rev("HEAD"))]
        );
    }
}
