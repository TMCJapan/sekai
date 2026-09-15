use core::fmt;

/// Error returned when parsing an invalid named code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseCodeError(pub ());

impl fmt::Display for ParseCodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid identifier or integer code")
    }
}

impl core::error::Error for ParseCodeError {}

/// Hex decoding error for 32-byte hashes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HexError {
    /// Hex input had a length other than 64 bytes (32-byte hash).
    BadHexLength(usize),
    /// Hex input contained a byte outside `[0-9a-fA-F]`.
    BadHexChar(u8),
}

impl fmt::Display for HexError {
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

impl core::error::Error for HexError {}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn error_messages() {
        assert_eq!(
            ParseCodeError(()).to_string(),
            "invalid identifier or integer code"
        );
        assert_eq!(
            HexError::BadHexLength(3).to_string(),
            "invalid hash hex length: 3, expected 64"
        );
        assert_eq!(
            HexError::BadHexChar(b'z').to_string(),
            "invalid hex character: 0x7A"
        );
    }
}
