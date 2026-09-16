//! Integration benchmarks for garbage collection and scanning.

#[path = "common.rs"]
mod common;

use common::{SMALL, corpus_world, generate, options, runtime, tempdir};
use criterion::{Criterion, criterion_group, criterion_main};
use sekai_app::Scope;

fn benches(c: &mut Criterion) {
    let rt = runtime();

    c.bench_function("gc-plan/small", |b| {
        let root = tempdir("gc-small");
        let world = root.join("world");
        generate(&world, &SMALL);
        let store = root.join("store").to_string_lossy().into_owned();
        rt.block_on(sekai_app::backup(
            &world,
            &store,
            options(),
            Scope::World,
            |_| {},
        ))
        .expect("backup works");
        // Plan is read-only and repeatable; apply (unlink syscalls) is
        // covered by unit tests.
        b.iter(|| {
            rt.block_on(sekai_app::gc_plan(&store))
                .expect("gc plan works");
        });
        std::fs::remove_dir_all(&root).ok();
    });

    c.bench_function("scan/corpus", |b| {
        let world = corpus_world();
        let root = world.parent().unwrap().to_path_buf();
        b.iter(|| {
            sekai_app::scan(&world).expect("scan works");
        });
        std::fs::remove_dir_all(&root).ok();
    });
}

criterion_group!(all_benches, benches);
criterion_main!(all_benches);
