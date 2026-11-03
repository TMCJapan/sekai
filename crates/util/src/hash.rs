//! Content hashes: the normalized chunk digest and its CAS key.

use alloc::string::String;

use super::error::HexError;

/// Digest of an arbitrary byte slice.
///
/// Only for payloads with no chunk schema to normalize (region file
/// fingerprints, non-level-chunk data); chunks hash through
/// `Chunk::normalized_hash`.
pub fn hash_bytes(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

/// Incremental Blake3 over a stream of byte slices.
///
/// Region files are fingerprinted while being read, so the whole file never
/// has to sit in memory.
#[derive(Debug, Default, Clone)]
pub struct BytesHasher(blake3::Hasher);

impl BytesHasher {
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

/// Blake3 key for normalized chunk content.
///
/// Only a hash of content constructs one; there is no "absent" digest, so an
/// absent chunk never compares equal to a present one by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChunkHash([u8; 32]);

/// Lowercase alphabet for CAS paths.
const HEX: &[u8; 16] = b"0123456789abcdef";

/// Decode one hexadecimal nibble.
const fn decode_nibble(byte: u8) -> Result<u8, HexError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        other => Err(HexError::BadHexChar(other)),
    }
}

/// Encode a 32-byte value as lowercase hex without allocation.
pub(crate) const fn hex32_into(bytes: [u8; 32]) -> [u8; 64] {
    let mut out = [0u8; 64];
    let mut i = 0;
    while i < 32 {
        out[i * 2] = HEX[(bytes[i] >> 4) as usize];
        out[i * 2 + 1] = HEX[(bytes[i] & 0x0F) as usize];
        i += 1;
    }
    out
}

/// Parse a 64-byte hexadecimal value into raw bytes.
pub(crate) fn hex32_from(input: &[u8]) -> Result<[u8; 32], HexError> {
    if input.len() != 64 {
        return Err(HexError::BadHexLength(input.len()));
    }
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        let hi = decode_nibble(input[i * 2])?;
        let lo = decode_nibble(input[i * 2 + 1])?;
        out[i] = (hi << 4) | lo;
        i += 1;
    }
    Ok(out)
}

impl ChunkHash {
    /// Wrap a chunk digest.
    #[inline]
    pub const fn new(hash: [u8; 32]) -> Self {
        Self(hash)
    }

    /// Raw 32 bytes (e.g. for binary DB columns).
    #[inline]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Encode as lowercase hex without allocation.
    pub const fn hex_into(self) -> [u8; 64] {
        hex32_into(self.0)
    }

    /// Encode as a lowercase hex string.
    pub fn hex_string(self) -> String {
        let raw = self.hex_into();
        String::from_utf8(raw.to_vec()).unwrap_or_default()
    }

    /// Parse a 64-byte hexadecimal value.
    pub fn from_hex(input: &[u8]) -> Result<Self, HexError> {
        hex32_from(input).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        let h = ChunkHash([7; 32]);
        let hex = h.hex_into();
        assert_eq!(ChunkHash::from_hex(&hex), Ok(h));
        let mut upper = hex;
        for b in &mut upper {
            *b = b.to_ascii_uppercase();
        }
        assert_eq!(ChunkHash::from_hex(&upper), Ok(h));
        assert_eq!(h.hex_string().as_bytes(), hex);
    }

    #[test]
    fn hex_rejects_bad_length_and_chars() {
        assert_eq!(ChunkHash::from_hex(b"abc"), Err(HexError::BadHexLength(3)));
        let mut bad = [b'0'; 64];
        bad[0] = b'z';
        assert_eq!(ChunkHash::from_hex(&bad), Err(HexError::BadHexChar(b'z')));
    }

    #[test]
    fn bytes_hasher_matches_a_single_shot_digest() {
        let data: alloc::vec::Vec<u8> = (0..300u32).map(|i| (i % 251) as u8).collect();
        let mut hasher = BytesHasher::new();
        for part in data.chunks(7) {
            hasher.update(part);
        }
        assert_eq!(hasher.finalize(), hash_bytes(&data));
        assert_eq!(BytesHasher::new().finalize(), hash_bytes(b""));
    }
}
