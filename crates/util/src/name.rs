//! Character grammar shared by ref and scope names.
//!
//! Each name type keeps its own error so messages say which kind of name
//! failed, and checks its own path shape; this module only decides which
//! shared rule a raw name breaks.

/// Longest accepted name, in bytes (names are ASCII-only).
pub(crate) const MAX_LEN: usize = 255;

/// First shared rule a raw name breaks.
pub(crate) enum NameRule {
    Empty,
    TooLong,
    BadChar(char),
}

/// Whether `c` may appear inside one `/`-separated component of a name:
/// an ASCII alphanumeric, `.`, `_`, or `-`.
pub(crate) const fn is_component_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
}

/// Check that `raw` is 1-[`MAX_LEN`] bytes of [component
/// characters](is_component_char) and `/`.
///
/// The byte-length check runs before the character walk, so an over-long
/// name reports `TooLong` even when it also holds a non-ASCII character.
pub(crate) fn check(raw: &str) -> Result<(), NameRule> {
    if raw.is_empty() {
        return Err(NameRule::Empty);
    }
    if raw.len() > MAX_LEN {
        return Err(NameRule::TooLong);
    }
    // Walk characters, not bytes: `BadChar` must name the rejected
    // character itself, not the first byte of its UTF-8 encoding.
    raw.chars()
        .find(|&c| !(is_component_char(c) || c == '/'))
        .map_or(Ok(()), |c| Err(NameRule::BadChar(c)))
}
