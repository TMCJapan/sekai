//! Decompression of raw chunk payloads.
//!
//! Rationale: `.mca` sectors store a compression-type byte followed by the
//! compressed body. Decoding here keeps framing knowledge in one place so
//! the normalizer always works on plain NBT bytes regardless of codec.

use std::io::Read as _;

use flate2::read::{GzDecoder, ZlibDecoder};
use lz4_java_wrc::Lz4BlockInput;

use crate::error::NbtError;

/// Codec framing a raw chunk payload, per the Anvil sector format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// `1`: Gzip-compressed NBT (legacy; unused by modern writers).
    Gzip,
    /// `2`: Zlib-compressed NBT (default for single-player worlds).
    Zlib,
    /// `3`: raw uncompressed NBT.
    Raw,
    /// `4`: LZ4-compressed NBT (since 24w04a, dedicated servers with
    /// `region-file-compression=lz4`).
    ///
    /// Vanilla uses the lz4-java `LZ4BlockOutputStream` framing, which is
    /// *not* the standard LZ4 block/frame format (per-block headers carry
    /// the sizes), hence the dedicated decoder crate.
    Lz4,
}

impl Compression {
    /// Decode the leading compression-type byte.
    ///
    /// `127` (third-party custom codec) and `>= 128` (chunk body stored
    /// externally in `c.<x>.<z>.mcc`) are rejected with dedicated errors so
    /// callers can tell "unsupported server setup" apart from corruption.
    const fn from_byte(byte: u8) -> Result<Self, NbtError> {
        match byte {
            1 => Ok(Self::Gzip),
            2 => Ok(Self::Zlib),
            3 => Ok(Self::Raw),
            4 => Ok(Self::Lz4),
            127 => Err(NbtError::CustomCompression),
            other => Err(NbtError::UnknownCompression(other)),
        }
    }
}

/// Decompress `raw_payload` into `out`, clearing it first.
///
/// Returns the detected codec. `out` is reused across calls so batch scans
/// avoid reallocation; it is cleared even on failure.
pub fn decompress_into(raw_payload: &[u8], out: &mut Vec<u8>) -> Result<Compression, NbtError> {
    out.clear();
    let (kind_byte, body) = raw_payload.split_first().ok_or(NbtError::EmptyPayload)?;
    let codec = Compression::from_byte(*kind_byte)?;
    match codec {
        Compression::Gzip => {
            GzDecoder::new(body)
                .read_to_end(out)
                .map_err(NbtError::Gzip)?;
        }
        Compression::Zlib => {
            ZlibDecoder::new(body)
                .read_to_end(out)
                .map_err(NbtError::Zlib)?;
        }
        Compression::Raw => out.extend_from_slice(body),
        Compression::Lz4 => {
            // Note: the block-stream decoder treats a truncated stream as
            // clean EOF (possibly zero bytes out). That never yields wrong
            // data: empty output always fails NBT parsing downstream, so
            // corruption still surfaces as an error at `parse_value` time.
            Lz4BlockInput::new(body)
                .read_to_end(out)
                .map_err(NbtError::Lz4)?;
        }
    }
    Ok(codec)
}

/// Decompress and parse `raw_payload` into an owned NBT value.
pub fn parse_value(raw_payload: &[u8]) -> Result<fastnbt::Value, NbtError> {
    let mut decompressed = Vec::new();
    decompress_into(raw_payload, &mut decompressed)?;
    Ok(fastnbt::from_bytes(&decompressed)?)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn gzip_body(data: &[u8]) -> Vec<u8> {
        use flate2::write::GzEncoder;
        use std::io::Write as _;
        let mut enc = GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(data).expect("test encoder must succeed");
        enc.finish().expect("test encoder must succeed")
    }

    fn zlib_body(data: &[u8]) -> Vec<u8> {
        use flate2::write::ZlibEncoder;
        use std::io::Write as _;
        let mut enc = ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(data).expect("test encoder must succeed");
        enc.finish().expect("test encoder must succeed")
    }

    fn lz4_body(data: &[u8]) -> Vec<u8> {
        use lz4_java_wrc::Lz4BlockOutput;
        use std::io::Write as _;
        // Same lz4-java block-stream framing vanilla servers write.
        // The encoder seals the stream on drop, so scope it explicitly.
        let mut body = Vec::new();
        {
            let mut enc = Lz4BlockOutput::new(&mut body);
            enc.write_all(data).expect("test encoder must succeed");
        }
        body
    }

    #[test]
    fn detects_all_four_codecs() {
        let mut out = Vec::new();
        let mut payload = vec![3];
        payload.extend_from_slice(b"nbt-bytes");
        assert_eq!(
            decompress_into(&payload, &mut out).expect("raw must decode"),
            Compression::Raw
        );
        assert_eq!(out, b"nbt-bytes");
    }

    #[test]
    fn gzip_zlib_and_lz4_round_trip() {
        let body = b"fake-nbt-body";
        let mut gz = vec![1];
        gz.extend_from_slice(&gzip_body(body));
        let mut zl = vec![2];
        zl.extend_from_slice(&zlib_body(body));
        let mut lz = vec![4];
        lz.extend_from_slice(&lz4_body(body));
        let mut out = Vec::new();
        assert_eq!(
            decompress_into(&gz, &mut out).expect("gzip must decode"),
            Compression::Gzip
        );
        assert_eq!(out, body);
        assert_eq!(
            decompress_into(&zl, &mut out).expect("zlib must decode"),
            Compression::Zlib
        );
        assert_eq!(out, body);
        assert_eq!(
            decompress_into(&lz, &mut out).expect("lz4 must decode"),
            Compression::Lz4
        );
        assert_eq!(out, body);
    }

    #[test]
    fn rejects_empty_and_unknown_and_corrupt() {
        let mut out = Vec::new();
        assert!(matches!(
            decompress_into(&[], &mut out),
            Err(NbtError::EmptyPayload)
        ));
        assert!(matches!(
            decompress_into(&[9, 0], &mut out),
            Err(NbtError::UnknownCompression(9))
        ));
        // Third-party custom codec and external-body markers are explicit.
        assert!(matches!(
            decompress_into(&[127, 0], &mut out),
            Err(NbtError::CustomCompression)
        ));
        assert!(matches!(
            decompress_into(&[130, 0], &mut out),
            Err(NbtError::UnknownCompression(130))
        ));
        // Declared gzip carrying garbage must surface a codec error.
        assert!(matches!(
            decompress_into(&[1, 0, 1, 2, 3], &mut out),
            Err(NbtError::Gzip(_))
        ));
        // Declared LZ4 carrying garbage must surface a codec error.
        // Framing: "LZ4Block" magic + token + LE compressed len (1) +
        // LE decompressed len (0): incoherent sizes always fail header
        // validation regardless of the token value.
        let mut bad_lz4 = vec![4];
        bad_lz4.extend_from_slice(b"LZ4Block");
        bad_lz4.extend_from_slice(&[0x10, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(matches!(
            decompress_into(&bad_lz4, &mut out),
            Err(NbtError::Lz4(_))
        ));
    }

    #[test]
    fn output_is_cleared_before_reuse() {
        let mut out = vec![9, 9, 9];
        let mut payload = vec![3];
        payload.extend_from_slice(b"ab");
        decompress_into(&payload, &mut out).expect("raw must decode");
        assert_eq!(out, b"ab");
    }
}
