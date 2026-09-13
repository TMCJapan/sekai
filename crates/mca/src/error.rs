//! Error type for region file decoding and atomic rewriting.
//!
//! Rationale: every corruption shape gets its own variant with the numbers
//! attached (entry index, offsets, lengths) so operators can tell damage
//! apart from unsupported setups at a glance. File-system failures carry
//! the affected path.

use std::io;
use std::path::PathBuf;

use sekai_core::{Dimension, RegionKind};

/// Failures while reading or rewriting `.mca` files.
#[derive(Debug, thiserror::Error)]
pub enum McaError {
    /// File-system operation failed.
    #[error("file I/O failed for {path}: {source}", path = path.display())]
    Io {
        /// File (or directory, for fsync) involved.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: io::Error,
    },

    /// File name was not `r.<x>.<z>.mca`.
    #[error("bad region file name (expected r.<x>.<z>.mca): {name}")]
    BadFilename {
        /// Offending file name.
        name: String,
    },

    /// Region coordinates cannot address chunk columns in `i32`.
    #[error("region coordinates out of range: r.{region_x}.{region_z}")]
    CoordinateOverflow {
        /// Parsed region X.
        region_x: i32,
        /// Parsed region Z.
        region_z: i32,
    },

    /// File is smaller than the 8 KiB header.
    #[error("truncated region file: {len} bytes, need at least 8192")]
    TruncatedFile {
        /// Observed file length.
        len: u64,
    },

    /// File length is not a multiple of the 4 KiB sector size.
    #[error("misaligned region file: {len} bytes is not a multiple of 4096")]
    MisalignedFile {
        /// Observed file length.
        len: u64,
    },

    /// Location-table entry is internally inconsistent or out of range.
    #[error("corrupt location entry {index}: offset {offset} sectors, {sectors} sectors")]
    CorruptEntry {
        /// Header slot `0..1024`.
        index: u32,
        /// Sector offset from the entry.
        offset: u32,
        /// Sector count from the entry.
        sectors: u32,
    },

    /// Length prefix disagrees with the allocated sectors or is zero.
    #[error("corrupt chunk payload at entry {index}: declared length {len}")]
    CorruptChunk {
        /// Header slot `0..1024`.
        index: u32,
        /// Declared payload length (type byte + body).
        len: u32,
    },

    /// Staged payload was empty (the compression-type byte is mandatory).
    #[error("empty chunk payload: missing compression-type byte")]
    EmptyPayload,

    /// Payload needs more than the 255 addressable sectors (~1 MiB).
    ///
    /// Larger-than-sector-file chunks (external `c.<x>.<z>.mcc` storage)
    /// are out of scope.
    #[error("chunk payload too large: {len} bytes need more than 255 sectors")]
    ChunkTooLarge {
        /// Staged payload length.
        len: usize,
    },

    /// Packed image would exceed the 24-bit sector-offset range.
    #[error("region image too large: {sectors} sectors exceed the offset range")]
    ImageTooLarge {
        /// Total sectors the staged chunks would need.
        sectors: u64,
    },

    /// Staged coordinate belongs to a different region file.
    #[error("chunk ({x}, {z}) does not belong to r.{region_x}.{region_z}")]
    WrongRegion {
        /// Region X of this file.
        region_x: i32,
        /// Region Z of this file.
        region_z: i32,
        /// Staged chunk X.
        x: i32,
        /// Staged chunk Z.
        z: i32,
    },

    /// No directory mapping exists for this coordinate's namespace.
    ///
    /// Vanilla namespaces are always mappable; this fires for hashed
    /// custom dimensions whose on-disk file is gone (the hash is one-way).
    #[error("cannot derive region path for dim {dim}, kind {kind}, r.{region_x}.{region_z}",
        dim = .dim.raw(), kind = .kind.raw())]
    UnknownRegionPath {
        /// Dimension namespace code.
        dim: Dimension,
        /// Region family code.
        kind: RegionKind,
        /// Region X.
        region_x: i32,
        /// Region Z.
        region_z: i32,
    },
}
