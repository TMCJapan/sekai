use crate::parser::Value;
use sekai_util::DiffHash;

pub const DEFAULT_IGNORED: &[&str] = &["InhabitedTime", "LastUpdate"];

pub(crate) trait Feed {
    fn update(&mut self, data: &[u8]);
}

impl Feed for blake3::Hasher {
    fn update(&mut self, data: &[u8]) {
        self.update(data);
    }
}

pub(crate) fn digest(ignored: &[&str], value: &Value) -> DiffHash {
    let mut hasher = blake3::Hasher::new();
    feed_value(&mut hasher, ignored, value);
    DiffHash(*hasher.finalize().as_bytes())
}

/// Saturating length cast: real NBT already bounds these, so saturation is
/// an unreachable-but-panic-free fallback that keeps the digest total.
fn len_u16(len: usize) -> [u8; 2] {
    u16::try_from(len).unwrap_or(u16::MAX).to_be_bytes()
}

/// Saturating length cast for array/list lengths (i32 in NBT spec).
fn len_i32(len: usize) -> [u8; 4] {
    i32::try_from(len).unwrap_or(i32::MAX).to_be_bytes()
}

fn feed_value(feed: &mut impl Feed, ignored: &[&str], value: &Value) {
    feed.update(&[value.tag_id()]);
    feed_payload(feed, ignored, value);
}

fn feed_payload(feed: &mut impl Feed, ignored: &[&str], value: &Value) {
    match value {
        Value::Byte(value) => feed.update(&value.to_be_bytes()),
        Value::Short(value) => feed.update(&value.to_be_bytes()),
        Value::Int(value) => feed.update(&value.to_be_bytes()),
        Value::Long(value) => feed.update(&value.to_be_bytes()),

        // Preserve the exact IEEE-754 representation, including NaN payloads.
        Value::Float(value) => feed.update(&value.to_bits().to_be_bytes()),
        Value::Double(value) => feed.update(&value.to_bits().to_be_bytes()),

        Value::String(value) => {
            feed.update(&len_u16(value.len()));
            feed.update(value.as_bytes());
        }

        Value::ByteArray(values) => {
            feed.update(&len_i32(values.len()));

            for &value in values {
                feed.update(&[value.cast_unsigned()]);
            }
        }

        Value::IntArray(values) => {
            feed.update(&len_i32(values.len()));

            for value in values {
                feed.update(&value.to_be_bytes());
            }
        }

        Value::LongArray(values) => {
            feed.update(&len_i32(values.len()));

            for value in values {
                feed.update(&value.to_be_bytes());
            }
        }

        Value::List(items) => {
            let element_tag = items.first().map_or(0, Value::tag_id);
            feed.update(&[element_tag]);
            feed.update(&len_i32(items.len()));

            for item in items {
                feed_value(feed, ignored, item);
            }
        }

        Value::Compound(entries) => {
            // Entries are sorted by the parser. Ignored names are omitted
            // recursively so volatile fields do not affect the digest.
            for (key, value) in entries {
                if ignored.contains(&key.as_str()) {
                    continue;
                }

                feed.update(&[value.tag_id()]);
                feed.update(&len_u16(key.len()));
                feed.update(key.as_bytes());
                feed_payload(feed, ignored, value);
            }

            feed.update(&[0]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_root;
    use alloc::borrow::ToOwned;
    use alloc::vec;

    fn chunk_value(last_update: i64, inhabited: i64, status: &str) -> Value {
        Value::Compound(vec![
            ("InhabitedTime".to_owned(), Value::Long(inhabited)),
            ("LastUpdate".to_owned(), Value::Long(last_update)),
            ("Status".to_owned(), Value::String(status.to_owned())),
            (
                "sections".to_owned(),
                Value::List(vec![Value::Compound(vec![(
                    "Y".to_owned(),
                    Value::Byte(0),
                )])]),
            ),
            ("xPos".to_owned(), Value::Int(3)),
        ])
    }

    #[test]
    fn default_rules_ignore_volatile_tags() {
        let a = digest(DEFAULT_IGNORED, &chunk_value(100, 42, "minecraft:full"));
        let b = digest(
            DEFAULT_IGNORED,
            &chunk_value(999_999, 10_000, "minecraft:full"),
        );

        assert_eq!(a, b);
    }

    #[test]
    fn meaningful_changes_affect_digest() {
        let a = digest(DEFAULT_IGNORED, &chunk_value(100, 42, "minecraft:full"));
        let b = digest(DEFAULT_IGNORED, &chunk_value(100, 42, "minecraft:empty"));

        assert_ne!(a, b);
    }

    #[test]
    fn custom_ignore_lists_apply() {
        assert_ne!(
            digest(&[], &chunk_value(1, 1, "a")),
            digest(&[], &chunk_value(2, 2, "a"))
        );

        assert_eq!(
            digest(&["Status"], &chunk_value(9, 9, "b")),
            digest(&["Status"], &chunk_value(9, 9, "c"))
        );

        assert_eq!(
            digest(
                &["LastUpdate", "InhabitedTime", "Status"],
                &chunk_value(1, 1, "a"),
            ),
            digest(
                &["LastUpdate", "InhabitedTime", "Status"],
                &chunk_value(2, 2, "b"),
            )
        );
    }

    #[test]
    fn parsed_values_digest_stably() {
        let bytes = vec![10, 0, 0, 3, 0, 1, b'a', 0, 0, 0, 7, 0];

        let a = parse_root(&bytes).unwrap();
        let b = parse_root(&bytes).unwrap();

        assert_eq!(digest(DEFAULT_IGNORED, &a), digest(DEFAULT_IGNORED, &b));
    }

    #[test]
    fn distinct_nan_payloads_digest_distinctly() {
        let a = Value::Float(f32::from_bits(0x7FC0_0001));
        let b = Value::Float(f32::from_bits(0x7FC0_0002));

        assert_ne!(digest(&[], &a), digest(&[], &b));
    }

    #[derive(Default)]
    struct FnvFeed(u64);

    impl FnvFeed {
        fn new() -> Self {
            Self(0xcbf2_9ce4_8422_2325)
        }
    }

    impl Feed for FnvFeed {
        fn update(&mut self, data: &[u8]) {
            for &byte in data {
                self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
            }
        }
    }

    fn fnv_digest(ignored: &[&str], value: &Value) -> u64 {
        let mut feed = FnvFeed::new();
        feed_value(&mut feed, ignored, value);
        feed.0
    }

    #[test]
    fn canonical_encoding_is_digest_independent() {
        let reference = fnv_digest(DEFAULT_IGNORED, &chunk_value(100, 42, "minecraft:full"));

        assert_eq!(
            reference,
            fnv_digest(
                DEFAULT_IGNORED,
                &chunk_value(999_999, 10_000, "minecraft:full"),
            )
        );

        assert_ne!(
            reference,
            fnv_digest(DEFAULT_IGNORED, &chunk_value(100, 42, "minecraft:empty"),)
        );
    }
}
