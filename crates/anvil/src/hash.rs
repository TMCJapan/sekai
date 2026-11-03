//! Hash helpers for already-read region data.

use sekai_util::Dimension;

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
    let b = sekai_util::hash_bytes(key);
    Dimension::new(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_ids_are_stable() {
        let a = custom_dimension_id("dimensions/aether/sky");

        assert_eq!(a, custom_dimension_id("dimensions/aether/sky",));

        assert_ne!(a, custom_dimension_id("dimensions/aether/other",));
    }

    /// `util` ships the trio ids as constants because hashing is not
    /// const-evaluable; pin them so they can never drift from the hash function.
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
