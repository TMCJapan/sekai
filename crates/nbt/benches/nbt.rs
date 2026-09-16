//! Micro benchmarks for NBT parsing, diffing, display, and digest.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use sekai_nbt::{DEFAULT_IGNORED, diff, diff_hash, parse};

fn chunk_nbt(status: &str, extra_sections: usize) -> Vec<u8> {
    let mut nbt = vec![10, 0, 0];
    nbt.extend_from_slice(b"\x08\x00\x06Status");
    nbt.extend_from_slice(&u16::try_from(status.len()).unwrap().to_be_bytes());
    nbt.extend_from_slice(status.as_bytes());
    nbt.extend_from_slice(b"\x09\x00\x08sections\x0a");
    nbt.extend_from_slice(&i32::try_from(extra_sections).unwrap().to_be_bytes());
    for i in 0..extra_sections {
        nbt.extend_from_slice(b"\x01\x00\x01Y");
        nbt.push(u8::try_from(i).unwrap());
        nbt.push(0);
    }
    nbt.push(0);
    nbt
}

fn benches(c: &mut Criterion) {
    let small = chunk_nbt("minecraft:full", 4);
    let large = chunk_nbt("minecraft:full", 64);
    let changed = chunk_nbt("minecraft:empty", 64);

    c.bench_function("parse/4-sections", |b| {
        b.iter(|| {
            parse(black_box(&small)).unwrap();
        });
    });
    c.bench_function("parse/64-sections", |b| {
        b.iter(|| {
            parse(black_box(&large)).unwrap();
        });
    });

    let old = parse(&large).unwrap();
    let new = parse(&changed).unwrap();
    c.bench_function("diff/64-sections", |b| {
        b.iter(|| {
            diff(black_box(&old), black_box(&new), DEFAULT_IGNORED);
        });
    });
    c.bench_function("display/64-sections", |b| {
        b.iter(|| {
            black_box(&new).to_string();
        });
    });
    c.bench_function("digest/64-sections", |b| {
        b.iter(|| {
            diff_hash(&large, DEFAULT_IGNORED).unwrap();
        });
    });
}

criterion_group!(all_benches, benches);
criterion_main!(all_benches);
