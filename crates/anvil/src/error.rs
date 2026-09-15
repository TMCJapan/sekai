//! Errors produced while parsing or rebuilding Anvil region images.

use alloc::string::String;
use core::fmt;

/// Errors produced while reading or rebuilding `.mca` images.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnvilError {
    /// File name was not `r.<x>.<z>.mca`.
    BadFilename { name: String },

    /// Region coordinates cannot address all 32 chunk columns.
    CoordinateOverflow { region_x: i32, region_z: i32 },

    /// Image is shorter than the 8 KiB header.
    TruncatedFile { len: u64 },

    /// A location-table entry is invalid or points outside the image.
    CorruptEntry {
        index: u32,
        offset: u32,
        sectors: u32,
    },

    /// A chunk length prefix is invalid for its allocated sectors.
    CorruptChunk { index: u32, len: u32 },

    /// A chunk payload has no compression-type byte.
    EmptyPayload,

    /// A chunk would require more than the 255 addressable sectors.
    ChunkTooLarge { len: usize },

    /// The rebuilt image exceeds the 24-bit sector-offset range.
    ImageTooLarge { sectors: u64 },

    /// A chunk coordinate does not belong to the selected region.
    WrongRegion {
        region_x: i32,
        region_z: i32,
        x: i32,
        z: i32,
    },

    /// The compression type is not recognized.
    UnknownCompression(u8),

    /// Type `127`: third-party custom compression.
    CustomCompression,

    /// Type `>= 128`: chunk data is stored in an external `.mcc` file.
    ExternalBody(u8),

    /// Gzip framing or decompression failed.
    Gzip,

    /// Zlib decompression failed.
    Zlib,

    /// LZ4-Java framing, decompression, or checksum validation failed.
    Lz4,

    /// Decompressed output exceeded the decoder safety limit.
    OutputTooLarge,
}

impl fmt::Display for AnvilError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadFilename { name } => {
                write!(f, "bad region file name (expected r.<x>.<z>.mca): {name}")
            }

            Self::CoordinateOverflow { region_x, region_z } => {
                write!(
                    f,
                    "region coordinates out of range: r.{region_x}.{region_z}"
                )
            }

            Self::TruncatedFile { len } => {
                write!(f, "truncated region image: {len} bytes, need at least 8192")
            }

            Self::CorruptEntry {
                index,
                offset,
                sectors,
            } => {
                write!(
                    f,
                    "corrupt location entry {index}: offset {offset} sectors, {sectors} sectors"
                )
            }

            Self::CorruptChunk { index, len } => {
                write!(
                    f,
                    "corrupt chunk payload at entry {index}: declared length {len}"
                )
            }

            Self::EmptyPayload => {
                write!(f, "empty chunk payload: missing compression-type byte")
            }

            Self::ChunkTooLarge { len } => {
                write!(
                    f,
                    "chunk payload too large: {len} bytes need more than 255 sectors"
                )
            }

            Self::ImageTooLarge { sectors } => {
                write!(
                    f,
                    "region image too large: {sectors} sectors exceed the offset range"
                )
            }

            Self::WrongRegion {
                region_x,
                region_z,
                x,
                z,
            } => {
                write!(
                    f,
                    "chunk ({x}, {z}) does not belong to r.{region_x}.{region_z}"
                )
            }

            Self::UnknownCompression(byte) => {
                write!(f, "unknown compression type: {byte}")
            }

            Self::CustomCompression => {
                write!(
                    f,
                    "custom compression (type 127) from third-party servers is unsupported"
                )
            }

            Self::ExternalBody(byte) => {
                write!(
                    f,
                    "external chunk body (type {byte}): payload lives in c.<x>.<z>.mcc, not the region file"
                )
            }

            Self::Gzip => write!(f, "gzip decode failed"),
            Self::Zlib => write!(f, "zlib decode failed"),
            Self::Lz4 => write!(f, "lz4 decode failed"),

            Self::OutputTooLarge => {
                write!(f, "decompressed output exceeded the safety bound")
            }
        }
    }
}

impl core::error::Error for AnvilError {}
