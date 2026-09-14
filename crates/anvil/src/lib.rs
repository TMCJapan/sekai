//! Pure Anvil sector codec, including decompression.
//!
//! Dependencies: `miniz_oxide` (gzip/zlib), `crc32fast` (gzip footer),
//! `lz4_flex` block API (lz4-java stream bodies), `twox-hash` (lz4-java
//! block checksums). All with `default-features = false` for `no_std`.
//!
//! # lz4-java `LZ4BlockOutputStream` framing spec
//!
//! Repeated blocks, each:
//!
//! ```text
//! offset  size  field
//! 0       8     magic: b"LZ4Block" (8 ASCII bytes)
//! 8       1     token: high nibble = method (0x10 Raw, 0x20 Lz4; else error),
//!                      low nibble = level L (max body = 1 << (10 + L), L in 0..=15)
//! 9       4     u32 LE compressed_len
//! 13      4     u32 LE decompressed_len
//! 17      4     u32 LE checksum (XXH32 of the *decompressed* body)
//! 21      N     body (N = compressed_len)
//! ```
//!
//! Validation per header:
//!
//! - `decompressed_len > max_body(level)` -> error.
//! - `compressed_len > i32::MAX` -> error.
//! - `(compressed_len == 0) != (decompressed_len == 0)` -> incoherent-size error.
//! - `Raw` requires `compressed_len == decompressed_len` -> error otherwise.
//! - Empty block (`0/0`) requires `checksum == 0` -> error otherwise.
//! - After decoding: `Raw` bodies are copied verbatim; `Lz4` bodies must
//!   decode to exactly `decompressed_len` bytes -> error otherwise.
//! - Checksum: `XXH32(seed = 0x9747b28c)` over the decompressed body, masked
//!   with `0x0fffffff` (the 4-top-bit-drop bug is part of the format -
//!   replicate it). Mismatch -> checksum error.
//! - Stream end: the first block with `decompressed_len == 0` ends the stream
//!   (`stop_on_empty_block` semantics); blocks after it are not read.

#![no_std]

extern crate alloc;
