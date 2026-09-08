//! Content hashes for the two-layer hashing model.
//!
//! Rationale: `blob_hash` (CAS key over exact raw bytes) and `diff_hash`
//! (volatile view over normalized NBT) share the same 32-byte width
//! (Blake3) but are distinct types so the compiler rejects mixing a
//! persistent restore key with an ephemeral change-detection value.
//! Hex helpers avoid allocation on hot paths via fixed `[u8; 64]` buffers.

use alloc::string::String;
use alloc::vec::Vec;

use super::error::CoreError;

/// Immutable CAS key: Blake3 over the exact raw chunk payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlobHash(pub [u8; 32]);

/// Volatile change-detection hash: Blake3 over normalized NBT.
///
/// Never used as a storage key and never persisted as restore metadata;
/// only history rows may cache it for fast comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DiffHash(pub [u8; 32]);

/// Lowercase hex alphabet for path-safe CAS file names.
const HEX: &[u8; 16] = b"0123456789abcdef";

/// Decode one hex nibble without panicking on invalid input.
const fn decode_nibble(byte: u8) -> Result<u8, CoreError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        other => Err(CoreError::BadHexChar(other)),
    }
}

impl BlobHash {
    /// Raw 32 bytes (e.g. for binary DB columns).
    #[inline]
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }

    /// Zero-allocation lowercase hex into a stack buffer (for CAS paths).
    #[must_use]
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

    /// Allocating hex string for display and directory/file names.
    #[must_use]
    pub fn hex_string(self) -> String {
        let raw = self.hex_into();
        let mut s: Vec<u8> = Vec::with_capacity(64);
        s.extend_from_slice(&raw);
        // Hex alphabet is ASCII by construction; validated at compile time.
        String::from_utf8(s).unwrap_or_default()
    }

    /// Parse 64 hex bytes (accepts either case).
    pub fn from_hex(input: &[u8]) -> Result<Self, CoreError> {
        if input.len() != 64 {
            return Err(CoreError::BadHexLength(input.len()));
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
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        let mut bytes = [0u8; 32];
        let mut i = 0;
        while i < 32 {
            bytes[i] = u8::try_from(i).unwrap_or_default();
            i += 1;
        }
        let h = BlobHash(bytes);
        let hex = h.hex_into();
        assert_eq!(BlobHash::from_hex(&hex), Ok(h));
        // Uppercase input is accepted too.
        let mut upper = hex;
        let mut j = 0;
        while j < 64 {
            upper[j] = upper[j].to_ascii_uppercase();
            j += 1;
        }
        assert_eq!(BlobHash::from_hex(&upper), Ok(h));
    }

    #[test]
    fn hex_rejects_bad_length_and_chars() {
        assert_eq!(BlobHash::from_hex(b"abc"), Err(CoreError::BadHexLength(3)));
        let mut bad = [b'0'; 64];
        bad[0] = b'z';
        assert_eq!(BlobHash::from_hex(&bad), Err(CoreError::BadHexChar(b'z')));
    }

    #[test]
    fn blob_and_diff_types_do_not_confuse() {
        // Compile-time guard: distinct types even with identical width.
        fn takes_blob(_: BlobHash) {}
        let d = DiffHash([0u8; 32]);
        let b = BlobHash(d.as_bytes());
        takes_blob(b);
    }
}
