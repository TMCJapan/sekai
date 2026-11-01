//! Micro benchmarks for chunk hashing.

use criterion::{Criterion, criterion_group, criterion_main};
use sekai_core::raw_hash;
use std::hint::black_box;

fn benches(c: &mut Criterion) {
    let tiny = vec![3u8; 64];
    let chunk = vec![7u8; 4096];
    let big = vec![9u8; 1_048_576];

    c.bench_function("hash/64B", |b| {
        b.iter(|| {
            raw_hash(black_box(&tiny));
        });
    });
    c.bench_function("hash/4KiB", |b| {
        b.iter(|| {
            raw_hash(black_box(&chunk));
        });
    });
    c.bench_function("hash/1MiB", |b| {
        b.iter(|| {
            raw_hash(black_box(&big));
        });
    });
}

criterion_group!(all_benches, benches);
criterion_main!(all_benches);
