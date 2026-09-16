//! Micro benchmarks for content hashing.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use sekai_core::hash_blob;

fn benches(c: &mut Criterion) {
    let tiny = vec![3u8; 64];
    let chunk = vec![7u8; 4096];
    let big = vec![9u8; 1_048_576];

    c.bench_function("hash/64B", |b| {
        b.iter(|| {
            hash_blob(black_box(&tiny));
        });
    });
    c.bench_function("hash/4KiB", |b| {
        b.iter(|| {
            hash_blob(black_box(&chunk));
        });
    });
    c.bench_function("hash/1MiB", |b| {
        b.iter(|| {
            hash_blob(black_box(&big));
        });
    });
}

criterion_group!(all_benches, benches);
criterion_main!(all_benches);
