//! Chunk compression framing and decompression.

use alloc::vec::Vec;
use core::hash::Hasher as _;

use miniz_oxide::inflate::{TINFLStatus, decompress_to_vec_with_limit};

use crate::error::AnvilError;

/// Compression formats supported by the Anvil chunk format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// `1`: Gzip-compressed NBT.
    Gzip,
    /// `2`: Zlib-compressed NBT.
    Zlib,
    /// `3`: Uncompressed NBT.
    Raw,
    /// `4`: Java's `LZ4BlockOutputStream` framing.
    Lz4,
}

/// Decodes an Anvil compression-type byte.
///
/// Types `127` and `128..=255` are reported separately because they indicate
/// unsupported storage schemes rather than an unknown built-in codec.
pub const fn compression_of(byte: u8) -> Result<Compression, AnvilError> {
    match byte {
        1 => Ok(Compression::Gzip),
        2 => Ok(Compression::Zlib),
        3 => Ok(Compression::Raw),
        4 => Ok(Compression::Lz4),
        127 => Err(AnvilError::CustomCompression),
        128..=u8::MAX => Err(AnvilError::ExternalBody(byte)),
        _ => Err(AnvilError::UnknownCompression(byte)),
    }
}

const MAX_DECOMPRESSED: usize = 1 << 26;

/// Decompresses `payload` into `out`, reusing its allocation.
///
/// `payload` consists of the one-byte compression type followed by the
/// encoded chunk body. `out` is cleared before decoding.
pub fn decompress_into(payload: &[u8], out: &mut Vec<u8>) -> Result<Compression, AnvilError> {
    out.clear();

    let (kind, body) = payload.split_first().ok_or(AnvilError::EmptyPayload)?;

    let codec = compression_of(*kind)?;

    match codec {
        Compression::Raw => {
            if body.len() > MAX_DECOMPRESSED {
                return Err(AnvilError::OutputTooLarge);
            }

            out.extend_from_slice(body);
        }
        Compression::Zlib => *out = inflate_zlib_capped(body)?,
        Compression::Gzip => gunzip_capped(body, out)?,
        Compression::Lz4 => unlz4(body, out)?,
    }

    Ok(codec)
}

fn inflate_zlib_capped(body: &[u8]) -> Result<Vec<u8>, AnvilError> {
    match miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(body, MAX_DECOMPRESSED) {
        Ok(out) => Ok(out),
        Err(err) if err.status == TINFLStatus::HasMoreOutput => Err(AnvilError::OutputTooLarge),
        Err(_) => Err(AnvilError::Zlib),
    }
}

fn inflate_raw_capped(body: &[u8]) -> Result<Vec<u8>, AnvilError> {
    match decompress_to_vec_with_limit(body, MAX_DECOMPRESSED) {
        Ok(out) => Ok(out),
        Err(err) if err.status == TINFLStatus::HasMoreOutput => Err(AnvilError::OutputTooLarge),
        Err(_) => Err(AnvilError::Gzip),
    }
}

fn read_u8(input: &[u8], pos: &mut usize) -> Result<u8, AnvilError> {
    let byte = input.get(*pos).copied().ok_or(AnvilError::Gzip)?;

    *pos += 1;
    Ok(byte)
}

fn read_u16_le(input: &[u8], pos: &mut usize) -> Result<u16, AnvilError> {
    let bytes = input.get(*pos..*pos + 2).ok_or(AnvilError::Gzip)?;

    *pos += 2;

    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn skip(input: &[u8], pos: &mut usize, len: usize) -> Result<(), AnvilError> {
    let end = pos.checked_add(len).ok_or(AnvilError::Gzip)?;

    if end > input.len() {
        return Err(AnvilError::Gzip);
    }

    *pos = end;
    Ok(())
}

fn gunzip_capped(body: &[u8], out: &mut Vec<u8>) -> Result<(), AnvilError> {
    let mut pos = 0;

    if read_u8(body, &mut pos)? != 0x1F
        || read_u8(body, &mut pos)? != 0x8B
        || read_u8(body, &mut pos)? != 8
    {
        return Err(AnvilError::Gzip);
    }

    let flags = read_u8(body, &mut pos)?;

    if flags & 0xE0 != 0 {
        return Err(AnvilError::Gzip);
    }

    skip(body, &mut pos, 6)?;

    if flags & 0x04 != 0 {
        let len = usize::from(read_u16_le(body, &mut pos)?);

        skip(body, &mut pos, len)?;
    }

    if flags & 0x08 != 0 {
        while read_u8(body, &mut pos)? != 0 {}
    }

    if flags & 0x10 != 0 {
        while read_u8(body, &mut pos)? != 0 {}
    }

    if flags & 0x02 != 0 {
        skip(body, &mut pos, 2)?;
    }

    let end = body.len().checked_sub(8).ok_or(AnvilError::Gzip)?;

    if pos > end {
        return Err(AnvilError::Gzip);
    }

    let stream = body.get(pos..end).ok_or(AnvilError::Gzip)?;

    let footer = body.get(end..).ok_or(AnvilError::Gzip)?;

    let decoded = inflate_raw_capped(stream)?;

    let mut crc = crc32fast::Hasher::new();
    crc.update(&decoded);

    let expected_crc = u32::from_le_bytes([footer[0], footer[1], footer[2], footer[3]]);

    if crc.finalize() != expected_crc {
        return Err(AnvilError::Gzip);
    }

    let isize = u32::from_le_bytes([footer[4], footer[5], footer[6], footer[7]]);

    if decoded.len() != isize as usize {
        return Err(AnvilError::Gzip);
    }

    out.extend_from_slice(&decoded);
    Ok(())
}

/// Decodes Java's `LZ4BlockOutputStream` framing.
///
/// This is not the standard LZ4 frame format; see the crate's `lz4.md`.
fn unlz4(body: &[u8], out: &mut Vec<u8>) -> Result<(), AnvilError> {
    const HEADER_LEN: usize = 21;
    const MAGIC: &[u8; 8] = b"LZ4Block";

    let mut pos = 0;

    loop {
        let header = body.get(pos..pos + HEADER_LEN).ok_or(AnvilError::Lz4)?;

        if &header[..8] != MAGIC {
            return Err(AnvilError::Lz4);
        }

        let token = header[8];
        let method = token & 0xF0;
        let level = token & 0x0F;

        if method != 0x10 && method != 0x20 {
            return Err(AnvilError::Lz4);
        }

        let compressed_len =
            u32::from_le_bytes([header[9], header[10], header[11], header[12]]) as usize;

        let decompressed_len =
            u32::from_le_bytes([header[13], header[14], header[15], header[16]]) as usize;

        let checksum = u32::from_le_bytes([header[17], header[18], header[19], header[20]]);

        let max_body = 1usize
            .checked_shl(10 + u32::from(level))
            .ok_or(AnvilError::Lz4)?;

        if decompressed_len > max_body {
            return Err(AnvilError::Lz4);
        }

        if (compressed_len == 0) != (decompressed_len == 0) {
            return Err(AnvilError::Lz4);
        }

        let raw = method == 0x10;

        if raw && compressed_len != decompressed_len {
            return Err(AnvilError::Lz4);
        }

        if decompressed_len == 0 {
            return if checksum == 0 {
                Ok(())
            } else {
                Err(AnvilError::Lz4)
            };
        }

        pos += HEADER_LEN;

        let start = out.len();

        let end_output = start
            .checked_add(decompressed_len)
            .ok_or(AnvilError::OutputTooLarge)?;

        if end_output > MAX_DECOMPRESSED {
            return Err(AnvilError::OutputTooLarge);
        }

        let body_end = pos.checked_add(compressed_len).ok_or(AnvilError::Lz4)?;

        let block = body.get(pos..body_end).ok_or(AnvilError::Lz4)?;

        pos = body_end;

        if raw {
            out.extend_from_slice(block);
        } else {
            out.resize(end_output, 0);

            let decoded = lz4_flex::block::decompress_into(
                block,
                out.get_mut(start..).ok_or(AnvilError::Lz4)?,
            )
            .map_err(|_| AnvilError::Lz4)?;

            if decoded != decompressed_len {
                out.truncate(start);
                return Err(AnvilError::Lz4);
            }
        }

        let mut xx = twox_hash::XxHash32::with_seed(0x9747_b28c);

        xx.write(out.get(start..).ok_or(AnvilError::Lz4)?);

        let computed = (xx.finish() & 0x0FFF_FFFF) as u32;

        if computed != checksum {
            out.truncate(start);
            return Err(AnvilError::Lz4);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn zlib_body(data: &[u8]) -> Vec<u8> {
        let mut payload = Vec::from([2]);

        payload.extend_from_slice(&miniz_oxide::deflate::compress_to_vec_zlib(data, 6));

        payload
    }

    fn gzip_body(data: &[u8], name: Option<&[u8]>) -> Vec<u8> {
        let mut payload = Vec::from([1, 0x1F, 0x8B, 0x08]);

        let flags = u8::from(name.is_some()) << 3;

        payload.push(flags);
        payload.extend_from_slice(&[0, 0, 0, 0, 0, 0]);

        if let Some(name) = name {
            payload.extend_from_slice(name);
            payload.push(0);
        }

        payload.extend_from_slice(&miniz_oxide::deflate::compress_to_vec(data, 6));

        let mut crc = crc32fast::Hasher::new();
        crc.update(data);

        payload.extend_from_slice(&crc.finalize().to_le_bytes());

        let isize = u32::try_from(data.len()).unwrap();

        payload.extend_from_slice(&isize.to_le_bytes());

        payload
    }

    fn lz4_checksum(body: &[u8]) -> [u8; 4] {
        let mut xx = twox_hash::XxHash32::with_seed(0x9747_b28c);

        xx.write(body);

        ((xx.finish() & 0x0FFF_FFFF) as u32).to_le_bytes()
    }

    fn lz4_block(token: u8, stored: &[u8], raw_len: u32, checksummed: &[u8]) -> Vec<u8> {
        let mut block = Vec::from(*b"LZ4Block");

        block.push(token);

        block.extend_from_slice(&u32::try_from(stored.len()).unwrap().to_le_bytes());

        block.extend_from_slice(&raw_len.to_le_bytes());

        block.extend_from_slice(&lz4_checksum(checksummed));

        block.extend_from_slice(stored);

        block
    }

    fn lz4_empty() -> Vec<u8> {
        let mut block = Vec::from(*b"LZ4Block");

        block.push(0x10);
        block.extend_from_slice(&[0; 12]);

        block
    }

    #[test]
    fn detects_all_four_codecs() {
        let mut out = Vec::new();
        let mut payload = Vec::from([3]);

        payload.extend_from_slice(b"nbt-bytes");

        assert_eq!(decompress_into(&payload, &mut out), Ok(Compression::Raw),);
        assert_eq!(out, b"nbt-bytes");
    }

    #[test]
    fn gzip_zlib_and_lz4_round_trip() {
        let body = b"fake-nbt-body";
        let mut out = Vec::new();

        assert_eq!(
            decompress_into(&zlib_body(body), &mut out,),
            Ok(Compression::Zlib),
        );
        assert_eq!(out, body);

        assert_eq!(
            decompress_into(&gzip_body(body, None), &mut out,),
            Ok(Compression::Gzip),
        );
        assert_eq!(out, body);

        assert_eq!(
            decompress_into(&gzip_body(body, Some(b"name")), &mut out,),
            Ok(Compression::Gzip),
        );
        assert_eq!(out, body);

        let raw_len = u32::try_from(body.len()).unwrap();

        let mut payload = Vec::from([4]);

        payload.extend_from_slice(&lz4_block(0x10, body, raw_len, body));
        payload.extend_from_slice(&lz4_empty());

        assert_eq!(decompress_into(&payload, &mut out), Ok(Compression::Lz4),);
        assert_eq!(out, body);

        let mut coded = Vec::from([4]);

        coded.extend_from_slice(&lz4_block(0x20, b"\x50hello", 5, b"hello"));
        coded.extend_from_slice(&lz4_empty());

        assert_eq!(decompress_into(&coded, &mut out), Ok(Compression::Lz4),);
        assert_eq!(out, b"hello");
    }

    #[test]
    fn rejects_empty_unknown_and_corrupt() {
        let mut out = Vec::new();

        assert_eq!(
            decompress_into(&[], &mut out),
            Err(AnvilError::EmptyPayload),
        );

        assert_eq!(
            decompress_into(&[9, 0], &mut out),
            Err(AnvilError::UnknownCompression(9)),
        );

        assert_eq!(
            decompress_into(&[127, 0], &mut out),
            Err(AnvilError::CustomCompression),
        );

        assert_eq!(
            decompress_into(&[128, 0], &mut out),
            Err(AnvilError::ExternalBody(128)),
        );

        assert_eq!(
            decompress_into(&[130, 0], &mut out),
            Err(AnvilError::ExternalBody(130)),
        );

        assert_eq!(
            decompress_into(&[1, 0, 1, 2, 3], &mut out,),
            Err(AnvilError::Gzip),
        );

        let mut bad = Vec::from([4]);
        bad.extend_from_slice(b"BADMagic");
        bad.extend_from_slice(&[0; 13]);

        assert_eq!(decompress_into(&bad, &mut out), Err(AnvilError::Lz4),);

        assert_eq!(decompress_into(&[4, 1], &mut out), Err(AnvilError::Lz4),);

        let mut payload = Vec::from([4]);
        let mut block = lz4_block(0x10, b"data", 4, b"data");

        let last = block.len() - 1;
        block[last] ^= 0xFF;

        payload.extend_from_slice(&block);
        payload.extend_from_slice(&lz4_empty());

        assert_eq!(decompress_into(&payload, &mut out), Err(AnvilError::Lz4),);
    }

    #[test]
    fn output_is_cleared_before_reuse() {
        let mut out = Vec::from([9, 9, 9]);
        let mut payload = Vec::from([3]);

        payload.extend_from_slice(b"ab");

        decompress_into(&payload, &mut out).unwrap();

        assert_eq!(out, b"ab");
    }
}
