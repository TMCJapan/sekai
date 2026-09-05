//! Error type for NBT decoding and normalization.
//!
//! Rationale: this crate performs I/O-adjacent work (decompression) and
//! codec work, so a `thiserror` enum carries each failure with its source.
//! Pure validation that needs no source uses unit variants.

/// Failures while deriving views from raw chunk payloads.
#[derive(Debug, thiserror::Error)]
pub enum NbtError {
    /// Payload was empty; at minimum the compression-type byte is required.
    #[error("empty chunk payload: missing compression-type byte")]
    EmptyPayload,

    /// First byte was not 1 (Gzip), 2 (Zlib), 3 (Uncompressed), or 4 (LZ4).
    ///
    /// Note: values `>= 128` mean the chunk body lives in an external
    /// `c.<x>.<z>.mcc` file rather than the region file, so the payload
    /// handed here was likely mis-sliced by the caller.
    #[error("unknown compression type: {0}")]
    UnknownCompression(u8),

    /// Type `127`: third-party custom codec (namespaced id follows).
    ///
    /// Out of scope: only vanilla codecs 1-4 are supported.
    #[error("custom compression (type 127) from third-party servers is unsupported")]
    CustomCompression,

    /// Gzip stream failed to decode.
    #[error("gzip decode failed")]
    Gzip(#[source] std::io::Error),

    /// Zlib stream failed to decode.
    #[error("zlib decode failed")]
    Zlib(#[source] std::io::Error),

    /// LZ4 (lz4-java block stream) failed to decode.
    #[error("lz4 decode failed")]
    Lz4(#[source] std::io::Error),

    /// Decompressed bytes were not valid NBT.
    #[error("nbt decode failed")]
    NbtDecode(#[from] fastnbt::error::Error),
}
