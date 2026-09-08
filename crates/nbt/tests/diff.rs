//! End-to-end diff-view guarantees through the `core::Normalizer` trait.
//!
//! Rationale: these tests pin the properties future phases rely on -
//! consumers must observe identical `DiffHash` for identical game state no
//! matter the on-disk key order or compression codec, while `LastUpdate`
//! churn alone must never surface as a change.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write as _;

use flate2::write::{GzEncoder, ZlibEncoder};
use sekai_core::Normalizer as _;
use sekai_nbt::NbtNormalizer;
use serde::Serialize;

/// Same fields, opposite declaration order: NBT byte order differs.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct ChunkA {
    status: String,
    last_update: i64,
    x_pos: i32,
}

/// Mirror of [`ChunkA`] with swapped field order.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct ChunkB {
    x_pos: i32,
    last_update: i64,
    status: String,
}

fn wrap(codec: u8, body: &[u8]) -> Vec<u8> {
    let mut payload = vec![codec];
    payload.extend_from_slice(body);
    payload
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut enc = GzEncoder::new(Vec::new(), flate2::Compression::fast());
    enc.write_all(data).unwrap();
    wrap(1, &enc.finish().unwrap())
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut enc = ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    enc.write_all(data).unwrap();
    wrap(2, &enc.finish().unwrap())
}

fn raw(data: &[u8]) -> Vec<u8> {
    wrap(3, data)
}

fn lz4(data: &[u8]) -> Vec<u8> {
    use lz4_java_wrc::Lz4BlockOutput;
    // Scoped so the encoder seals the block stream on drop.
    let mut body = Vec::new();
    {
        let mut enc = Lz4BlockOutput::new(&mut body);
        enc.write_all(data).unwrap();
    }
    wrap(4, &body)
}

#[test]
fn key_order_and_codec_do_not_affect_diff() {
    let rules = NbtNormalizer::v1();
    let a = ChunkA {
        status: "minecraft:full".to_string(),
        last_update: 111,
        x_pos: 5,
    };
    let b = ChunkB {
        x_pos: 5,
        last_update: 999_999,
        status: "minecraft:full".to_string(),
    };
    let bytes_a = fastnbt::to_bytes(&a).unwrap();
    let bytes_b = fastnbt::to_bytes(&b).unwrap();
    // Sanity: the serialized forms really do differ (order + value).
    assert_ne!(bytes_a, bytes_b);

    let reference = rules.diff_hash(&gzip(&bytes_a)).unwrap();
    assert_eq!(reference, rules.diff_hash(&gzip(&bytes_b)).unwrap());
    assert_eq!(reference, rules.diff_hash(&zlib(&bytes_a)).unwrap());
    assert_eq!(reference, rules.diff_hash(&raw(&bytes_a)).unwrap());
    assert_eq!(reference, rules.diff_hash(&lz4(&bytes_a)).unwrap());
}

#[test]
fn real_change_is_detected() {
    let rules = NbtNormalizer::v1();
    let base = ChunkA {
        status: "minecraft:full".to_string(),
        last_update: 1,
        x_pos: 5,
    };
    let moved = ChunkA {
        status: "minecraft:full".to_string(),
        last_update: 1,
        x_pos: 6,
    };
    assert_ne!(
        rules
            .diff_hash(&gzip(&fastnbt::to_bytes(&base).unwrap()))
            .unwrap(),
        rules
            .diff_hash(&gzip(&fastnbt::to_bytes(&moved).unwrap()))
            .unwrap()
    );
}

#[test]
fn corrupt_body_surfaces_decode_error() {
    let rules = NbtNormalizer::v1();
    let mut payload = gzip(b"this is not nbt");
    // Sanity that the framing itself is intact: codec byte + valid gzip.
    assert_eq!(payload[0], 1);
    assert!(rules.diff_hash(&payload).is_err());
    payload[0] = 7;
    assert!(rules.diff_hash(&payload).is_err());
    assert!(rules.diff_hash(&[]).is_err());
}
