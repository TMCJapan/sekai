//! Content hashes for the two-layer hashing model.
//!
//! Persistent blob hashes and volatile diff hashes.

use alloc::string::String;

use super::error::HexError;

/// Blake3 key for exact raw chunk payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlobHash(pub [u8; 32]);

/// Blake3 hash of normalized NBT used for change detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DiffHash(pub [u8; 32]);

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

impl BlobHash {
    /// Raw 32 bytes (e.g. for binary DB columns).
    #[inline]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }

    /// Encode as lowercase hex without allocation.
    pub const fn hex_into(self) -> [u8; 64] {
        let mut out = [0u8; 64];
        let mut i = 0;
        while i < 32 {
            out[i * 2] = HEX[(self.0[i] >> 4) as usize];
            out[i * 2 + 1] = HEX[(self.0[i] & 0x0F) as usize];
            i += 1;
        }
        out
    }

    /// Encode as a lowercase hex string.
    pub fn hex_string(self) -> String {
        let raw = self.hex_into();
        // SAFETY: hex_into produces only lowercase hex ASCII bytes.
        unsafe { String::from_utf8_unchecked(raw.to_vec()) }
    }

    /// Parse a 64-byte hexadecimal value.
    pub fn from_hex(input: &[u8]) -> Result<Self, HexError> {
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
        Ok(Self(out))
    }
}

impl DiffHash {
    /// Raw 32 bytes.
    #[inline]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        let h = BlobHash([7; 32]);
        let hex = h.hex_into();
        assert_eq!(BlobHash::from_hex(&hex), Ok(h));
        let mut upper = hex;
        for b in &mut upper {
            *b = b.to_ascii_uppercase();
        }
        assert_eq!(BlobHash::from_hex(&upper), Ok(h));
        assert_eq!(h.hex_string().as_bytes(), hex);
    }

    #[test]
    fn hex_rejects_bad_length_and_chars() {
        assert_eq!(BlobHash::from_hex(b"abc"), Err(HexError::BadHexLength(3)));
        let mut bad = [b'0'; 64];
        bad[0] = b'z';
        assert_eq!(BlobHash::from_hex(&bad), Err(HexError::BadHexChar(b'z')));
    }
}
