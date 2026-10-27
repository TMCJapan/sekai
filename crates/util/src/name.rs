//! Path-like name grammar shared by ref and scope names.
//!
//! Each name type keeps its own error so messages say which kind of name
//! failed; this module only decides which rule a raw name breaks.

/// Longest accepted path-like name, in bytes (names are ASCII-only).
pub(crate) const MAX_LEN: usize = 64;

/// First grammar rule a raw name breaks.
pub(crate) enum NameRule {
    Empty,
    TooLong,
    BadChar(char),
    BadPath(&'static str),
    Numeric,
}

/// Check `raw` against the path-like grammar: 1-[`MAX_LEN`] bytes of
/// ASCII alphanumerics plus `.`, `_`, `-`, and `/`, not all digits, with
/// no empty component and no `.` or `..` segment.
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
    let mut all_digits = true;
    // Walk characters, not bytes: `BadChar` must name the rejected
    // character itself, not the first byte of its UTF-8 encoding.
    for c in raw.chars() {
        let valid = c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/');
        if !valid {
            return Err(NameRule::BadChar(c));
        }
        all_digits &= c.is_ascii_digit();
    }
    if all_digits {
        return Err(NameRule::Numeric);
    }
    if raw.starts_with('/') || raw.ends_with('/') || raw.contains("//") {
        return Err(NameRule::BadPath("empty path component"));
    }
    if raw.split('/').any(|segment| matches!(segment, "." | "..")) {
        return Err(NameRule::BadPath("'.' or '..' segment"));
    }
    Ok(())
}
