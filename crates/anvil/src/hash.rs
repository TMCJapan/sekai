//! Hash helpers for already-read region data.

use sekai_util::Dimension;

/// Blake3 digest of a region-file header or other byte prefix.
pub fn header_hash(header: &[u8]) -> [u8; 32] {
    *blake3::hash(header).as_bytes()
}

/// Stable dimension identifier derived from a custom dimension path.
pub fn custom_dimension_id(relative: &str) -> Dimension {
    let digest = blake3::hash(relative.as_bytes());

    let b = digest.as_bytes();
    Dimension::new(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_hash_is_stable() {
        assert_eq!(header_hash(b"abc"), header_hash(b"abc"));
        assert_ne!(header_hash(b"abc"), header_hash(b"xyz"));
    }

    #[test]
    fn custom_ids_are_stable() {
        let a = custom_dimension_id("dimensions/aether/sky");

        assert_eq!(a, custom_dimension_id("dimensions/aether/sky",));

        assert_ne!(a, custom_dimension_id("dimensions/aether/other",));
    }
}
