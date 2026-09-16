//! Integration benchmarks for rollback flows.

#[path = "common.rs"]
mod common;

use common::{SMALL, corpus_world, generate, options, runtime, tempdir};
use criterion::{Criterion, criterion_group, criterion_main};
use sekai_app::Scope;

fn benches(c: &mut Criterion) {
    let rt = runtime();

    c.bench_function("rollback/small", |b| {
        let root = tempdir("rollback-small");
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
        let snapshots = rt
            .block_on(sekai_app::list_snapshots(&store))
            .expect("list works");
        b.iter(|| {
            rt.block_on(sekai_app::rollback(
                &world,
                &store,
                snapshots[0].id,
                Scope::World,
                |_| {},
            ))
            .expect("rollback works");
        });
        std::fs::remove_dir_all(&root).ok();
    });

    c.bench_function("rollback/corpus", |b| {
        let world = corpus_world();
        let root = world.parent().unwrap().to_path_buf();
        let store = root.join("store").to_string_lossy().into_owned();
        rt.block_on(sekai_app::backup(
            &world,
            &store,
            options(),
            Scope::World,
            |_| {},
        ))
        .expect("backup works");
        let snapshots = rt
            .block_on(sekai_app::list_snapshots(&store))
            .expect("list works");
        b.iter(|| {
            rt.block_on(sekai_app::rollback(
                &world,
                &store,
                snapshots[0].id,
                Scope::World,
                |_| {},
            ))
            .expect("rollback works");
        });
        std::fs::remove_dir_all(&root).ok();
    });
}

criterion_group!(all_benches, benches);
criterion_main!(all_benches);
