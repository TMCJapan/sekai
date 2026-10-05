//! Integration benchmarks for full backup flows.

#[path = "common.rs"]
mod common;

use common::{MEDIUM, SMALL, generate, options, runtime, tempdir};
use criterion::{Criterion, criterion_group, criterion_main};
use sekai_app::{HostWorldTree, Scope};

fn benches(c: &mut Criterion) {
    let rt = runtime();

    c.bench_function("backup/small-full", |b| {
        b.iter(|| {
            let root = tempdir("small");
            let mut world = HostWorldTree::new(root.join("world"));
            generate(&world, &SMALL);
            let store = root.join("store").to_string_lossy().into_owned();
            let mut instance = rt
                .block_on(sekai_app::SekaiInstance::init(&store))
                .expect("open works");
            rt.block_on(
                instance
                    .world_mut(&mut world)
                    .backup(options(), Scope::World, |_| {}),
            )
            .expect("backup works");
            std::fs::remove_dir_all(&root).ok();
        });
    });

    c.bench_function("backup/small-incremental", |b| {
        let root = tempdir("incr");
        let mut world = HostWorldTree::new(root.join("world"));
        generate(&world, &SMALL);
        let store = root.join("store").to_string_lossy().into_owned();
        let mut instance = rt
            .block_on(sekai_app::SekaiInstance::init(&store))
            .expect("open works");
        rt.block_on(
            instance
                .world_mut(&mut world)
                .backup(options(), Scope::World, |_| {}),
        )
        .expect("backup works");
        b.iter(|| {
            rt.block_on(
                instance
                    .world_mut(&mut world)
                    .backup(options(), Scope::World, |_| {}),
            )
            .expect("backup works");
        });
        std::fs::remove_dir_all(&root).ok();
    });

    c.bench_function("backup/medium-full", |b| {
        b.iter(|| {
            let root = tempdir("medium");
            let mut world = HostWorldTree::new(root.join("world"));
            generate(&world, &MEDIUM);
            let store = root.join("store").to_string_lossy().into_owned();
            let mut instance = rt
                .block_on(sekai_app::SekaiInstance::init(&store))
                .expect("open works");
            rt.block_on(
                instance
                    .world_mut(&mut world)
                    .backup(options(), Scope::World, |_| {}),
            )
            .expect("backup works");
            std::fs::remove_dir_all(&root).ok();
        });
    });
}

criterion_group!(all_benches, benches);
criterion_main!(all_benches);
