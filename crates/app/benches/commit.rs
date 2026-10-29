//! Integration benchmarks for full commit flows.

#[path = "common.rs"]
mod common;

use common::{MEDIUM, SMALL, generate, options, runtime, tempdir};
use criterion::{Criterion, criterion_group, criterion_main};
use sekai_app::{HostWorldTree, Scope};

fn benches(c: &mut Criterion) {
    let rt = runtime();

    c.bench_function("commit/small-full", |b| {
        b.iter(|| {
            let root = tempdir("small");
            let world_path = root.path().join("world");
            let mut world = HostWorldTree::new(&world_path).expect("world resolves");
            generate(&world_path, &SMALL);
            let store = root.path().join("store").to_string_lossy().into_owned();
            let mut instance = rt
                .block_on(sekai_app::SekaiInstance::init(&store))
                .expect("open works");
            rt.block_on(
                instance
                    .world_mut(&mut world)
                    .commit(options(), Scope::World, |_| {}),
            )
            .expect("commit works");
        });
    });

    c.bench_function("commit/small-incremental", |b| {
        let root = tempdir("incr");
        let world_path = root.path().join("world");
        let mut world = HostWorldTree::new(&world_path).expect("world resolves");
        generate(&world_path, &SMALL);
        let store = root.path().join("store").to_string_lossy().into_owned();
        let mut instance = rt
            .block_on(sekai_app::SekaiInstance::init(&store))
            .expect("open works");
        rt.block_on(
            instance
                .world_mut(&mut world)
                .commit(options(), Scope::World, |_| {}),
        )
        .expect("commit works");
        b.iter(|| {
            rt.block_on(
                instance
                    .world_mut(&mut world)
                    .commit(options(), Scope::World, |_| {}),
            )
            .expect("commit works");
        });
    });

    c.bench_function("commit/medium-full", |b| {
        b.iter(|| {
            let root = tempdir("medium");
            let world_path = root.path().join("world");
            let mut world = HostWorldTree::new(&world_path).expect("world resolves");
            generate(&world_path, &MEDIUM);
            let store = root.path().join("store").to_string_lossy().into_owned();
            let mut instance = rt
                .block_on(sekai_app::SekaiInstance::init(&store))
                .expect("open works");
            rt.block_on(
                instance
                    .world_mut(&mut world)
                    .commit(options(), Scope::World, |_| {}),
            )
            .expect("commit works");
        });
    });
}

criterion_group!(all_benches, benches);
criterion_main!(all_benches);
