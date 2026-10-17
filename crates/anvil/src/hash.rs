//! Hash helpers for already-read region data.

use sekai_util::Dimension;

/// Blake3 digest of a byte slice.
pub fn content_hash(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

/// Incremental Blake3 over a stream of byte slices.
///
/// Region files are hashed while being read, so the whole file never has to
/// sit in memory. The primitive lives here because compression framing and
/// digests are Anvil knowledge; `world` only supplies the bytes.
#[derive(Debug, Default, Clone)]
pub struct ContentHasher(blake3::Hasher);

impl ContentHasher {
    /// Fresh hasher.
    pub fn new() -> Self {
        Self(blake3::Hasher::new())
    }

    /// Feed the next slice of content.
    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    /// Consume the hasher and return the digest.
    pub fn finalize(self) -> [u8; 32] {
        *self.0.finalize().as_bytes()
    }
}

/// Stable dimension identifier derived from a dimension's canonical
/// `<namespace>:<name>` key.
///
/// Every observed dimension is keyed this way, the vanilla trio included:
/// `custom_dimension_id("minecraft:overworld")` is [`Dimension::OVERWORLD`].
pub fn custom_dimension_id(key: &str) -> Dimension {
    custom_dimension_id_bytes(key.as_bytes())
}

/// [`custom_dimension_id`] over raw key bytes.
///
/// Used for keys built from directory names that are not valid UTF-8; for
/// valid UTF-8 the bytes are the string's own, so both entry points agree.
pub fn custom_dimension_id_bytes(key: &[u8]) -> Dimension {
    let digest = blake3::hash(key);

    let b = digest.as_bytes();
    Dimension::new(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_is_stable() {
        assert_eq!(content_hash(b"abc"), content_hash(b"abc"));
        assert_ne!(content_hash(b"abc"), content_hash(b"xyz"));
    }

    /// Streaming in pieces must equal hashing the whole slice at once.
    #[test]
    fn content_hasher_matches_a_single_shot_digest() {
        let data: alloc::vec::Vec<u8> = (0..300u32).map(|i| (i % 251) as u8).collect();
        let mut hasher = ContentHasher::new();
        for chunk in data.chunks(7) {
            hasher.update(chunk);
        }
        assert_eq!(hasher.finalize(), content_hash(&data));

        let empty = ContentHasher::new();
        assert_eq!(empty.finalize(), content_hash(b""));
    }

    #[test]
    fn custom_ids_are_stable() {
        let a = custom_dimension_id("dimensions/aether/sky");

        assert_eq!(a, custom_dimension_id("dimensions/aether/sky",));

        assert_ne!(a, custom_dimension_id("dimensions/aether/other",));
    }

    /// `util` ships the trio ids precomputed because it cannot run blake3;
    /// pin them so they can never drift from the hash function.
    #[test]
    fn vanilla_trio_consts_match_hashed_keys() {
        assert_eq!(
            Dimension::OVERWORLD,
            custom_dimension_id("minecraft:overworld")
        );
        assert_eq!(
            Dimension::NETHER,
            custom_dimension_id("minecraft:the_nether")
        );
        assert_eq!(Dimension::END, custom_dimension_id("minecraft:the_end"));
    }

    /// Raw-byte keys hash like string keys, and a non-UTF-8 name must not
    /// collapse onto its lossy spelling.
    #[test]
    fn raw_byte_keys_match_string_keys() {
        assert_eq!(
            custom_dimension_id_bytes(b"aether:sky"),
            custom_dimension_id("aether:sky")
        );
        assert_ne!(
            custom_dimension_id_bytes(b"aether:sky-\xff"),
            custom_dimension_id_bytes("aether:sky-\u{fffd}".as_bytes())
        );
    }
}
