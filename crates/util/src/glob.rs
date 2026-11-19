//! Git's `wildmatch`, without `WM_PATHNAME`: the glob `branch --list`
//! and `tag -l` match against short names (git 2.53).

/// Whether `text` matches the glob `pattern`, as Git's `wildmatch`
/// without `WM_PATHNAME` matches `branch --list` and `tag -l` patterns
/// against short names (git 2.53).
///
/// `*` matches any run of characters, `/` included, `?` one character,
/// `[...]` one character of a set or range, `[!...]` and `[^...]` one
/// outside it, and `\` quotes the next character. POSIX classes such as
/// `[[:alpha:]]` are not supported.
pub fn wildmatch(pattern: &str, text: &str) -> bool {
    glob(pattern.as_bytes(), text.as_bytes()) == Outcome::Match
}

/// How a pattern fares against a text, as Git's `dowild` reports it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Match,
    NoMatch,
    /// The text ran out before the pattern did. A `*` further out cannot
    /// do better by starting later, so the whole search stops, as Git's
    /// `WM_ABORT_ALL` does; without it, a pattern with many `*` would
    /// backtrack exponentially.
    Abort,
}

fn glob(pattern: &[u8], text: &[u8]) -> Outcome {
    let Some((&first, rest)) = pattern.split_first() else {
        return if text.is_empty() {
            Outcome::Match
        } else {
            Outcome::NoMatch
        };
    };
    if first == b'*' {
        return (0..=text.len())
            .filter_map(|skip| text.get(skip..))
            .map(|tail| glob(rest, tail))
            .find(|&outcome| outcome != Outcome::NoMatch)
            .unwrap_or(Outcome::Abort);
    }
    let Some((&c, tail)) = text.split_first() else {
        return Outcome::Abort;
    };
    let (hit, rest) = match first {
        b'?' => (true, rest),
        // An unclosed `[` is an ordinary character.
        b'[' => class(rest, c).unwrap_or((c == b'[', rest)),
        b'\\' => match rest.split_first() {
            Some((&quoted, rest)) => (c == quoted, rest),
            None => (c == b'\\', rest),
        },
        literal => (c == literal, rest),
    };
    if hit {
        glob(rest, tail)
    } else {
        Outcome::NoMatch
    }
}

/// Whether `c` falls in the class that `pattern` opens, just past its
/// `[`, and the pattern after the closing `]`; `None` when no `]` closes
/// it.
fn class(pattern: &[u8], c: u8) -> Option<(bool, &[u8])> {
    let (negated, mut rest) = match pattern.split_first() {
        Some((b'!' | b'^', rest)) => (true, rest),
        _ => (false, pattern),
    };
    let mut hit = false;
    let mut first = true;
    loop {
        let (&start, after) = rest.split_first()?;
        if start == b']' && !first {
            return Some((hit != negated, after));
        }
        first = false;
        let (start, after) = match (start, after.split_first()) {
            (b'\\', Some((&quoted, after))) => (quoted, after),
            _ => (start, after),
        };
        rest = match after {
            // A quoted range end, as in `[a-\z]`, compares unquoted.
            [b'-', b'\\', end, after @ ..] => {
                hit |= (start..=*end).contains(&c);
                after
            }
            [b'-', end, after @ ..] if *end != b']' => {
                hit |= (start..=*end).contains(&c);
                after
            }
            _ => {
                hit |= start == c;
                after
            }
        };
    }
}

#[cfg(test)]
mod glob_tests {
    use super::wildmatch;

    #[test]
    fn globs_match_as_git_branch_list_does() {
        for (pattern, text, expected) in [
            ("main", "main", true),
            ("ma", "main", false),
            ("*", "feature/x", true),
            ("feat*", "feature/x", true),
            ("*/x", "feature/x", true),
            ("v?", "v1", true),
            ("v?", "v10", false),
            ("v[0-9]", "v7", true),
            ("v[!0-9]", "v7", false),
            ("v[^a]", "vb", true),
            ("v[]]", "v]", true),
            ("a\\*", "a*", true),
            ("a\\*", "ab", false),
            ("[ab", "[ab", true),
            ("v[a-\\z]", "vm", true),
            ("*a*b", "xaab", true),
            ("*a*b", "xaa", false),
        ] {
            assert_eq!(wildmatch(pattern, text), expected, "{pattern} vs {text}");
        }
    }

    #[test]
    fn many_stars_stop_once_the_text_runs_out() {
        let text = "a".repeat(255);
        assert!(!wildmatch("*a*a*a*a*a*a*a*a*a*a*b", &text));
        assert!(wildmatch("*a*a*a*a*a*a*a*a*a*a", &text));
    }
}
