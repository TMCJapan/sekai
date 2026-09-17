//! Micro benchmarks for chunk decompression and image visiting.

use criterion::{Criterion, criterion_group, criterion_main};
use sekai_anvil::{RegionBuilder, RegionImage, decompress_into};

fn fixture_payload(compression: u8, body: &[u8]) -> Vec<u8> {
    let mut payload = vec![compression];
    payload.extend_from_slice(body);
    payload
}

fn zlib_body(data: &[u8]) -> Vec<u8> {
    fixture_payload(2, &miniz_oxide::deflate::compress_to_vec_zlib(data, 6))
}

fn raw_body(data: &[u8]) -> Vec<u8> {
    fixture_payload(3, data)
}

fn sample_nbt(size: usize) -> Vec<u8> {
    // Plausible chunk NBT: compound root with string and int-array leaves.
    let mut nbt = vec![10, 0, 0];
    nbt.extend_from_slice(b"\x08\x00\x06Status");
    nbt.extend_from_slice(&u16::try_from(size).unwrap().to_be_bytes());
    nbt.extend(core::iter::repeat_n(b'x', size));
    nbt.extend_from_slice(b"\x0b\x00\x04Data");
    nbt.extend_from_slice(&7i32.to_be_bytes());
    for i in 0..7i32 {
        nbt.extend_from_slice(&i.to_be_bytes());
    }
    nbt.push(0);
    nbt
}

fn dense_image(chunks: usize) -> Vec<u8> {
    let mut builder = RegionBuilder::new(0, 0, 0).unwrap();
    let payload = zlib_body(&sample_nbt(512));
    for i in 0..chunks {
        builder
            .stage_chunk(
                i32::try_from(i).unwrap() % 32,
                i32::try_from(i).unwrap() / 32,
                &payload,
            )
            .unwrap();
    }
    builder.image().unwrap()
}

fn benches(c: &mut Criterion) {
    let small = sample_nbt(64);
    let large = sample_nbt(8192);
    let zlib_small = zlib_body(&small);
    let zlib_large = zlib_body(&large);
    let raw_small = raw_body(&small);
    let mut out = Vec::new();

    c.bench_function("decompress/zlib-64B", |b| {
        b.iter(|| {
            decompress_into(&zlib_small, &mut out).unwrap();
        });
    });
    c.bench_function("decompress/zlib-8KiB", |b| {
        b.iter(|| {
            decompress_into(&zlib_large, &mut out).unwrap();
        });
    });
    c.bench_function("decompress/raw-64B", |b| {
        b.iter(|| {
            decompress_into(&raw_small, &mut out).unwrap();
        });
    });

    let image = dense_image(256);
    c.bench_function("visit/256-chunks", |b| {
        b.iter(|| {
            let image = RegionImage::from_bytes(image.clone(), 0, 0).unwrap();
            let mut count = 0usize;
            image
                .visit_chunks(|_| {
                    count += 1;
                    true
                })
                .unwrap();
            assert_eq!(count, 256);
        });
    });
}

criterion_group!(all_benches, benches);
criterion_main!(all_benches);
