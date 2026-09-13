//! Core validation errors.
//!
//! Rationale: `core` is `no_std` with zero dependencies, so it cannot use
//! `thiserror`/`anyhow`. Outer crates map their own I/O and codec failures
//! onto associated `Error` types in the traits; `CoreError` only covers
//! pure value-type validation (hex parsing, struct invariants) that has no
//! I/O involved.

use core::fmt;

/// Validation failure for pure value types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreError {
    /// Hex input had a length other than 64 bytes (32-byte hash).
    BadHexLength(usize),
    /// Hex input contained a byte outside `[0-9a-fA-F]`.
    BadHexChar(u8),
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadHexLength(len) => {
                write!(f, "invalid hash hex length: {len}, expected 64")
            }
            Self::BadHexChar(byte) => {
                write!(f, "invalid hex character: {byte:#04X}")
            }
        }
    }
}
