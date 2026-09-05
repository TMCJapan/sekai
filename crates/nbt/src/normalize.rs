//! Volatile diff views over chunk NBT.
//!
//! Rationale: `fastnbt::Value::Compound` is backed by a `HashMap`, so
//! re-serializing a parsed value yields nondeterministic key order. The
//! digest therefore never hashes serialized bytes; instead it streams a
//! canonical encoding (compounds key-sorted, big-endian scalars) directly
//! into Blake3. Ignored volatile tags (v1: `LastUpdate`) are skipped at any
//! depth, which is change-detection-equivalent to zero-clearing while
//! staying type-agnostic.

use fastnbt::{Tag, Value};
use sekai_core::{DiffHash, DiffHasher, Normalizer};

use crate::codec::parse_value;
use crate::error::NbtError;

/// Computes [`DiffHash`] views; implements [`Normalizer`].
///
/// V1 rules ignore `LastUpdate`. Future rule versions only change which
/// names are skipped, never stored blobs or CAS keys.
#[derive(Debug, Clone)]
pub struct NbtNormalizer {
    /// Tag names skipped at any depth during the canonical feed.
    ignored: Vec<String>,
}

impl NbtNormalizer {
    /// V1 normalization rules.
    #[must_use]
    pub fn v1() -> Self {
        Self {
            ignored: vec!["LastUpdate".to_string()],
        }
    }

    /// Additionally ignore `name` at any depth. Idempotent.
    #[must_use]
    pub fn ignore(mut self, name: &str) -> Self {
        if !self.ignored.iter().any(|n| n == name) {
            self.ignored.push(name.to_string());
        }
        self
    }

    /// Whether `name` is skipped by the current rules.
    fn is_ignored(&self, name: &str) -> bool {
        self.ignored.iter().any(|n| n == name)
    }

    /// Digest an already-parsed value (exposed for testing and reuse).
    #[must_use]
    pub fn canonical_digest(&self, value: &Value) -> DiffHash {
        self.canonical_digest_with::<Blake3Feed>(value)
    }

    /// Digest with an injected hasher (test seam: a non-Blake3 digest pins
    /// the canonical encoding without depending on the digest function).
    #[must_use]
    pub fn canonical_digest_with<H: DiffHasher>(&self, value: &Value) -> DiffHash {
        let mut hasher = H::new();
        feed_value(&mut hasher, self, value);
        hasher.finalize()
    }
}

/// Blake3 behind the [`DiffHasher`] seam (the only digest in this crate).
///
/// The canonical feed never names this type: it streams into any
/// `DiffHasher`, so the encoding stays testable without Blake3.
#[derive(Debug, Clone)]
struct Blake3Feed {
    /// Inner streaming state.
    inner: blake3::Hasher,
}

impl DiffHasher for Blake3Feed {
    fn new() -> Self {
        Self {
            inner: blake3::Hasher::new(),
        }
    }

    fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    fn finalize(self) -> DiffHash {
        DiffHash(*self.inner.finalize().as_bytes())
    }
}

impl Default for NbtNormalizer {
    fn default() -> Self {
        Self::v1()
    }
}

impl Normalizer for NbtNormalizer {
    type Error = NbtError;

    fn diff_hash(&self, raw_payload: &[u8]) -> Result<DiffHash, Self::Error> {
        Ok(self.canonical_digest(&parse_value(raw_payload)?))
    }
}

/// Stable one-byte discriminant per tag (mirrors the NBT spec IDs).
const fn tag_byte(tag: Tag) -> u8 {
    match tag {
        Tag::End => 0,
        Tag::Byte => 1,
        Tag::Short => 2,
        Tag::Int => 3,
        Tag::Long => 4,
        Tag::Float => 5,
        Tag::Double => 6,
        Tag::ByteArray => 7,
        Tag::String => 8,
        Tag::List => 9,
        Tag::Compound => 10,
        Tag::IntArray => 11,
        Tag::LongArray => 12,
    }
}

/// Tag discriminant of a value (lists report `List` regardless of content).
const fn tag_of(value: &Value) -> Tag {
    match value {
        Value::Byte(_) => Tag::Byte,
        Value::Short(_) => Tag::Short,
        Value::Int(_) => Tag::Int,
        Value::Long(_) => Tag::Long,
        Value::Float(_) => Tag::Float,
        Value::Double(_) => Tag::Double,
        Value::String(_) => Tag::String,
        Value::ByteArray(_) => Tag::ByteArray,
        Value::IntArray(_) => Tag::IntArray,
        Value::LongArray(_) => Tag::LongArray,
        Value::List(_) => Tag::List,
        Value::Compound(_) => Tag::Compound,
    }
}

/// Saturating length casts: real NBT already bounds these, so saturation is
/// an unreachable-but-panic-free fallback that keeps the digest total.
fn len_u16(len: usize) -> [u8; 2] {
    u16::try_from(len).unwrap_or(u16::MAX).to_be_bytes()
}

fn len_i32(len: usize) -> [u8; 4] {
    i32::try_from(len).unwrap_or(i32::MAX).to_be_bytes()
}

/// Feed one value with its tag discriminant (used for root and elements).
fn feed_value(hasher: &mut impl DiffHasher, rules: &NbtNormalizer, value: &Value) {
    hasher.update(&[tag_byte(tag_of(value))]);
    feed_payload(hasher, rules, value);
}

/// Feed a value body without tag or name (entries/elements add framing).
fn feed_payload(hasher: &mut impl DiffHasher, rules: &NbtNormalizer, value: &Value) {
    match value {
        Value::Byte(v) => {
            hasher.update(&v.to_be_bytes());
        }
        Value::Short(v) => {
            hasher.update(&v.to_be_bytes());
        }
        Value::Int(v) => {
            hasher.update(&v.to_be_bytes());
        }
        Value::Long(v) => {
            hasher.update(&v.to_be_bytes());
        }
        // Bitwise semantics: distinct NaN payloads digest distinctly.
        // Conservative for change detection (no false negatives).
        Value::Float(v) => {
            hasher.update(&v.to_bits().to_be_bytes());
        }
        Value::Double(v) => {
            hasher.update(&v.to_bits().to_be_bytes());
        }
        // Canonical form is plain UTF-8, not Java CESU-8; equality is
        // preserved (same string always yields same bytes).
        Value::String(v) => {
            hasher.update(&len_u16(v.len()));
            hasher.update(v.as_bytes());
        }
        Value::ByteArray(v) => {
            hasher.update(&len_i32(v.len()));
            for b in v.iter() {
                hasher.update(&b.to_be_bytes());
            }
        }
        Value::IntArray(v) => {
            hasher.update(&len_i32(v.len()));
            for i in v.iter() {
                hasher.update(&i.to_be_bytes());
            }
        }
        Value::LongArray(v) => {
            hasher.update(&len_i32(v.len()));
            for l in v.iter() {
                hasher.update(&l.to_be_bytes());
            }
        }
        Value::List(items) => {
            // Header mirrors NBT (element type + length); each element is
            // then fed tagged so heterogeneous lists stay deterministic.
            let elem = items.first().map_or(Tag::End, tag_of);
            hasher.update(&[tag_byte(elem)]);
            hasher.update(&len_i32(items.len()));
            for item in items {
                feed_value(hasher, rules, item);
            }
        }
        Value::Compound(map) => {
            // Key-sorted names make HashMap iteration order irrelevant.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for key in keys {
                if rules.is_ignored(key) {
                    continue;
                }
                if let Some(child) = map.get(key) {
                    hasher.update(&[tag_byte(tag_of(child))]);
                    hasher.update(&len_u16(key.len()));
                    hasher.update(key.as_bytes());
                    feed_payload(hasher, rules, child);
                }
            }
            hasher.update(&[tag_byte(Tag::End)]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk_nbt(last_update: i64, status: &str) -> Value {
        fastnbt::nbt!({
            "Status": status,
            "xPos": 3,
            "zPos": -7,
            "LastUpdate": last_update,
            "InhabitedTime": 42i64,
            "sections": [
                {"Y": -1i8, "block_states": {"palette": [{"Name": "minecraft:stone"}]}},
                {"Y": 0i8}
            ],
            "entities": [],
            "block_entities": [{"id": "minecraft:chest", "x": 1, "y": 2, "z": 3}]
        })
    }

    #[test]
    fn volatile_tags_do_not_affect_digest() {
        let rules = NbtNormalizer::v1();
        let a = rules.canonical_digest(&chunk_nbt(100, "minecraft:full"));
        let b = rules.canonical_digest(&chunk_nbt(999_999, "minecraft:full"));
        assert_eq!(a, b);
    }

    #[test]
    fn meaningful_changes_affect_digest() {
        let rules = NbtNormalizer::v1();
        let a = rules.canonical_digest(&chunk_nbt(100, "minecraft:full"));
        let b = rules.canonical_digest(&chunk_nbt(100, "minecraft:empty"));
        assert_ne!(a, b);
    }

    #[test]
    fn digest_is_stable_across_hashmap_orders() {
        // `Value::Compound` is a HashMap; identical content must digest
        // identically no matter the iteration order. A hundred iterations
        // make an order-dependent feed flaky enough to catch.
        let rules = NbtNormalizer::v1();
        let reference = rules.canonical_digest(&chunk_nbt(7, "minecraft:full"));
        for _ in 0..100 {
            assert_eq!(
                reference,
                rules.canonical_digest(&chunk_nbt(7, "minecraft:full"))
            );
        }
    }

    #[test]
    fn custom_ignore_rules_apply() {
        let base = NbtNormalizer::v1();
        let extended = NbtNormalizer::v1().ignore("InhabitedTime");
        let a = chunk_nbt(7, "minecraft:full");
        let mut altered = a.clone();
        if let Value::Compound(map) = &mut altered {
            map.insert("InhabitedTime".to_string(), Value::Long(10_000));
        }
        // Base rules see the difference; extended rules do not.
        assert_ne!(base.canonical_digest(&a), base.canonical_digest(&altered));
        assert_eq!(
            extended.canonical_digest(&a),
            extended.canonical_digest(&altered)
        );
    }

    /// Non-cryptographic digest behind the `DiffHasher` seam: proves the
    /// canonical encoding feeds deterministically through any hasher.
    #[derive(Default)]
    struct FnvFeed(u64);

    impl DiffHasher for FnvFeed {
        fn new() -> Self {
            Self(0xcbf2_9ce4_8422_2325)
        }

        fn update(&mut self, data: &[u8]) {
            for byte in data {
                self.0 ^= u64::from(*byte);
                self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
            }
        }

        fn finalize(self) -> DiffHash {
            let mut out = [0u8; 32];
            out[..8].copy_from_slice(&self.0.to_be_bytes());
            DiffHash(out)
        }
    }

    #[test]
    fn canonical_encoding_is_digest_independent() {
        let rules = NbtNormalizer::v1();
        let reference = rules.canonical_digest_with::<FnvFeed>(&chunk_nbt(100, "minecraft:full"));
        // Volatile tags stay ignored without Blake3 in the loop.
        assert_eq!(
            reference,
            rules.canonical_digest_with::<FnvFeed>(&chunk_nbt(999_999, "minecraft:full"))
        );
        // Meaningful changes still surface through the seam.
        assert_ne!(
            reference,
            rules.canonical_digest_with::<FnvFeed>(&chunk_nbt(100, "minecraft:empty"))
        );
    }
}
