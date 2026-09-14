//! Validation errors for pure value types.

use core::fmt;

/// Validation error for pure value types.
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

impl core::error::Error for CoreError {}
