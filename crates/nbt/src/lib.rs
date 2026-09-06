#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! NBT decoding, decompression, and volatile diff views.
//!
//! Rationale: chunk payloads arrive exactly as stored in `.mca` sectors
//! (compression-type byte followed by compressed data) and are preserved
//! verbatim into CAS by other crates. This crate only derives ephemeral
//! views: it decompresses, parses, and feeds a canonical digest into
//! [`sekai_core::DiffHasher`] output without ever mutating stored bytes.

mod codec;
mod error;
mod normalize;

pub use codec::{Compression, decompress_into, parse_value};
pub use error::NbtError;
pub use normalize::NbtNormalizer;
