use core::fmt;

/// Errors produced while parsing NBT data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NbtError {
    EmptyInput,
    UnexpectedEnd,
    UnknownTag(u8),
    InvalidString,
    UnexpectedRoot(u8),
    TrailingData(usize),
    TooDeeplyNested,
    InvalidLength(i32),
}

impl fmt::Display for NbtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyInput => write!(f, "empty NBT input: missing root tag"),
            Self::UnexpectedEnd => write!(f, "truncated NBT input"),
            Self::UnknownTag(tag) => write!(f, "unknown NBT tag id: {tag}"),
            Self::InvalidString => write!(f, "invalid modified UTF-8 in NBT string"),
            Self::UnexpectedRoot(tag) => {
                write!(
                    f,
                    "unexpected NBT root tag id: {tag}, expected compound (10)"
                )
            }
            Self::TrailingData(len) => {
                write!(f, "trailing {len} bytes after NBT root value")
            }
            Self::TooDeeplyNested => {
                write!(f, "NBT nesting exceeds the recursion bound")
            }
            Self::InvalidLength(len) => {
                write!(f, "invalid NBT length prefix: {len}")
            }
        }
    }
}

impl core::error::Error for NbtError {}
