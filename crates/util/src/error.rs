//! Errors shared by the parsing and encoding helpers.

use core::fmt;

use crate::name::NameRule;

impl From<NameRule> for RefNameError {
    fn from(rule: NameRule) -> Self {
        match rule {
            NameRule::Empty => Self::Empty,
            NameRule::TooLong => Self::TooLong(crate::name::MAX_LEN),
            NameRule::BadChar(c) => Self::BadChar(c),
        }
    }
}

impl From<NameRule> for ScopeNameError {
    fn from(rule: NameRule) -> Self {
        match rule {
            NameRule::Empty => Self::Empty,
            NameRule::TooLong => Self::TooLong(crate::name::MAX_LEN),
            NameRule::BadChar(c) => Self::BadChar(c),
        }
    }
}

impl From<NameRule> for WorktreeNameError {
    fn from(rule: NameRule) -> Self {
        match rule {
            NameRule::Empty => Self::Empty,
            NameRule::TooLong => Self::TooLong(crate::name::MAX_LEN),
            NameRule::BadChar(c) => Self::BadChar(c),
        }
    }
}

/// Error returned when parsing an invalid named code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseCodeError(pub ());

impl fmt::Display for ParseCodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid identifier or integer code")
    }
}

impl core::error::Error for ParseCodeError {}

/// Hex decoding error for 32-byte hashes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HexError {
    /// Hex input had a length other than 64 bytes (32-byte hash).
    BadHexLength(usize),
    /// Hex input contained a byte outside `[0-9a-fA-F]`.
    BadHexChar(u8),
}

impl fmt::Display for HexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadHexLength(len) => write!(f, "invalid hash hex length: {len}, expected 64"),
            Self::BadHexChar(byte) => write!(f, "invalid hex character: {byte:#04X}"),
        }
    }
}

impl core::error::Error for HexError {}

/// Ref name validation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefNameError {
    /// Name was empty.
    Empty,
    /// Name exceeded the limit; the payload is that limit in bytes
    /// (`RefName::MAX_LEN`).
    TooLong(usize),
    /// Name held a character outside ASCII alphanumerics, `.`, `_`, `-`,
    /// or `/`.
    BadChar(char),
    /// Name has an invalid path shape; the payload is the violated rule
    /// (an empty component, a `..`, or a trailing `.`).
    BadPath(&'static str),
    /// Name started with `-`, which reads as an option on a command line.
    LeadingDash,
    /// Name is reserved; the payload is the reserved spelling
    /// (`RefName::RESERVED`).
    Reserved(&'static str),
    /// A full refname lay outside `refs/heads/` and `refs/tags/`.
    UnknownNamespace,
}

impl fmt::Display for RefNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "ref name is empty"),
            Self::TooLong(limit) => write!(f, "ref name exceeds {limit} bytes"),
            Self::BadChar(c) => write!(f, "ref name holds invalid character: {c:?}"),
            Self::BadPath(reason) => {
                write!(f, "ref name has an invalid path shape: {reason}")
            }
            Self::LeadingDash => write!(f, "ref name starts with '-'"),
            Self::Reserved(name) => write!(f, "ref name {name:?} is reserved"),
            Self::UnknownNamespace => {
                write!(f, "refname is not under refs/heads/ or refs/tags/")
            }
        }
    }
}

impl core::error::Error for RefNameError {}

/// Scope name validation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeNameError {
    /// Name was empty.
    Empty,
    /// Name exceeded the limit; the payload is that limit in bytes
    /// (`ScopeName::MAX_LEN`).
    TooLong(usize),
    /// Name held a character outside ASCII alphanumerics, `.`, `_`, `-`,
    /// or `/`.
    BadChar(char),
}

impl fmt::Display for ScopeNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "scope name is empty"),
            Self::TooLong(limit) => write!(f, "scope name exceeds {limit} bytes"),
            Self::BadChar(c) => write!(f, "scope name holds invalid character: {c:?}"),
        }
    }
}

impl core::error::Error for ScopeNameError {}

/// Worktree name validation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeNameError {
    /// Name was empty.
    Empty,
    /// Name exceeded the limit; the payload is that limit in bytes
    /// (`WorktreeName::MAX_LEN`).
    TooLong(usize),
    /// Name held a character outside ASCII alphanumerics, `.`, `_`, or
    /// `-`.
    BadChar(char),
    /// Name contains `..`.
    DoubleDot,
}

impl fmt::Display for WorktreeNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "worktree name is empty"),
            Self::TooLong(limit) => write!(f, "worktree name exceeds {limit} bytes"),
            Self::BadChar(c) => write!(f, "worktree name holds invalid character: {c:?}"),
            Self::DoubleDot => write!(f, "worktree name contains '..'"),
        }
    }
}

impl core::error::Error for WorktreeNameError {}

/// A revision outside the grammar ADR-0036 adopts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionError {
    /// The revision, or the part before its steps, is empty.
    Empty,
    /// The starting point is neither `HEAD`, `@`, nor a ref name or
    /// commit ID.
    Name(RefNameError),
    /// A step is not `~<n>`.
    BadStep,
    /// `<a>...<b>` is not adopted.
    SymmetricDifference,
}

impl fmt::Display for RevisionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "revision is empty"),
            Self::Name(err) => write!(f, "not a commit ID or ref name: {err}"),
            Self::BadStep => write!(f, "expected ~<n> after the starting point"),
            Self::SymmetricDifference => write!(f, "<a>...<b> is not supported"),
        }
    }
}

impl core::error::Error for RevisionError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Name(err) => Some(err),
            _ => None,
        }
    }
}

/// A named scope record that would select nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedScopeError {
    /// The scope names no area.
    NoAreas,
}

impl fmt::Display for NamedScopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoAreas => write!(f, "scope selects no area"),
        }
    }
}

impl core::error::Error for NamedScopeError {}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn error_messages() {
        assert_eq!(
            ParseCodeError(()).to_string(),
            "invalid identifier or integer code"
        );
        assert_eq!(
            HexError::BadHexLength(3).to_string(),
            "invalid hash hex length: 3, expected 64"
        );
        assert_eq!(
            HexError::BadHexChar(b'z').to_string(),
            "invalid hex character: 0x7A"
        );
    }
}
