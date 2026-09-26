//! SNBT (Stringified NBT) rendering for [`Value`](super::Value).
//!
//! Rendering is display-only: nothing here parses SNBT back.

use alloc::string::ToString;
use core::fmt;

use super::Value;

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Byte(v) => write!(f, "{v}b"),
            Self::Short(v) => write!(f, "{v}s"),
            Self::Int(v) => write!(f, "{v}"),
            Self::Long(v) => write!(f, "{v}L"),
            // `to_string` on the value itself: widening an f32 to f64
            // first would print the exact binary64 expansion (`0.1` as
            // `0.10000000149011612`) instead of the shortest f32 form.
            Self::Float(v) => fmt_float(&v.to_string(), v.is_finite(), f, "f"),
            Self::Double(v) => fmt_float(&v.to_string(), v.is_finite(), f, ""),
            Self::ByteArray(items) => {
                write!(f, "[B;")?;
                join(f, items.iter(), |f, v| write!(f, "{v}"))?;
                write!(f, "]")
            }
            Self::String(v) => write!(f, "\"{}\"", Quoted(v)),
            Self::List { items, .. } => {
                write!(f, "[")?;
                join(f, items.iter(), |f, v| write!(f, "{v}"))?;
                write!(f, "]")
            }
            Self::Compound(entries) => {
                write!(f, "{{")?;
                join(f, entries.iter(), |f, (key, value)| {
                    write!(f, "{}: {value}", Key(key))
                })?;
                write!(f, "}}")
            }
            Self::IntArray(items) => {
                write!(f, "[I;")?;
                join(f, items.iter(), |f, v| write!(f, "{v}"))?;
                write!(f, "]")
            }
            Self::LongArray(items) => {
                write!(f, "[L;")?;
                join(f, items.iter(), |f, v| write!(f, "{v}L"))?;
                write!(f, "]")
            }
        }
    }
}

/// Comma-separated items.
fn join<T>(
    f: &mut fmt::Formatter<'_>,
    items: impl Iterator<Item = T>,
    mut write: impl FnMut(&mut fmt::Formatter<'_>, T) -> fmt::Result,
) -> fmt::Result {
    let mut first = true;
    for item in items {
        if !first {
            write!(f, ", ")?;
        }
        first = false;
        write(f, item)?;
    }
    Ok(())
}

/// Float formatting that always carries a decimal point (`1.0f`, not `1f`),
/// so integers and floats stay visually distinct. Non-finite values pass
/// through Rust-style (`NaNf`, `inff`); SNBT cannot represent them.
fn fmt_float(text: &str, finite: bool, f: &mut fmt::Formatter<'_>, suffix: &str) -> fmt::Result {
    write!(f, "{text}")?;
    if finite && !text.contains(['.', 'e', 'E']) {
        write!(f, ".0")?;
    }
    write!(f, "{suffix}")
}

/// Double-quoted string with `"` and `\` escapes.
struct Quoted<'a>(&'a str);

impl fmt::Display for Quoted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for c in self.0.chars() {
            match c {
                '"' => write!(f, "\\\"")?,
                '\\' => write!(f, "\\\\")?,
                c => write!(f, "{c}")?,
            }
        }
        Ok(())
    }
}

/// Compound key: bare when it matches `[A-Za-z0-9._+-]+`, quoted otherwise.
struct Key<'a>(&'a str);

impl fmt::Display for Key<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bare = !self.0.is_empty()
            && self
                .0
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'));
        if bare {
            write!(f, "{}", self.0)
        } else {
            write!(f, "\"{}\"", Quoted(self.0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::vec;

    fn rendered(value: &Value) -> String {
        value.to_string()
    }

    #[test]
    fn renders_scalars() {
        assert_eq!(rendered(&Value::Byte(-3)), "-3b");
        assert_eq!(rendered(&Value::Short(300)), "300s");
        assert_eq!(rendered(&Value::Int(3)), "3");
        assert_eq!(rendered(&Value::Long(-7)), "-7L");
        assert_eq!(rendered(&Value::Float(1.5)), "1.5f");
        assert_eq!(rendered(&Value::Float(1.0)), "1.0f");
        assert_eq!(rendered(&Value::Double(2.25)), "2.25");
        assert_eq!(rendered(&Value::Double(2.0)), "2.0");
    }

    /// An `f32` must render at `f32` precision: widening it to `f64` first
    /// would print the exact binary64 expansion of the value.
    #[test]
    fn renders_floats_at_their_own_precision() {
        assert_eq!(rendered(&Value::Float(0.1)), "0.1f");
        assert_eq!(rendered(&Value::Float(0.3)), "0.3f");
        assert_eq!(rendered(&Value::Float(1.0 / 3.0)), "0.33333334f");
        assert_eq!(rendered(&Value::Double(0.1)), "0.1");
    }

    #[test]
    fn renders_strings_with_escapes() {
        assert_eq!(rendered(&Value::String("foo".into())), "\"foo\"");
        assert_eq!(
            rendered(&Value::String("a\"b\\c".into())),
            "\"a\\\"b\\\\c\""
        );
    }

    #[test]
    fn renders_arrays() {
        assert_eq!(rendered(&Value::ByteArray(vec![1, -2])), "[B;1, -2]");
        assert_eq!(rendered(&Value::ByteArray(vec![])), "[B;]");
        assert_eq!(rendered(&Value::IntArray(vec![1, 2])), "[I;1, 2]");
        assert_eq!(rendered(&Value::LongArray(vec![1])), "[L;1L]");
    }

    #[test]
    fn renders_nested_compounds_and_lists() {
        let value = Value::Compound(vec![
            ("Status".into(), Value::String("full".into())),
            (
                "sections".into(),
                Value::list(vec![Value::Compound(vec![("Y".into(), Value::Byte(0))])]),
            ),
        ]);
        assert_eq!(rendered(&value), "{Status: \"full\", sections: [{Y: 0b}]}");
    }

    #[test]
    fn quotes_special_keys() {
        let value = Value::Compound(vec![
            ("a b".into(), Value::Int(1)),
            (String::new(), Value::Int(2)),
            ("a-b_c.d+e".into(), Value::Int(3)),
        ]);
        assert_eq!(rendered(&value), "{\"a b\": 1, \"\": 2, a-b_c.d+e: 3}");
    }

    #[test]
    fn renders_non_finite_floats() {
        assert_eq!(rendered(&Value::Float(f32::NAN)), "NaNf");
        assert_eq!(rendered(&Value::Float(f32::INFINITY)), "inff");
        assert_eq!(rendered(&Value::Double(f64::NEG_INFINITY)), "-inf");
    }
}
