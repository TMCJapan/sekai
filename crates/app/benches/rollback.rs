//! Integration benchmarks for rollback flows.

#[path = "common.rs"]
mod common;

use common::{SMALL, corpus_world, generate, options, runtime, tempdir};
use criterion::{Criterion, criterion_group, criterion_main};
use sekai_app::{HostWorldTree, Scope};

fn benches(c: &mut Criterion) {
    let rt = runtime();

    c.bench_function("rollback/small", |b| {
        let root = tempdir("rollback-small");
        let world_path = root.join("world");
        let mut world = HostWorldTree::new(&world_path);
        generate(&world_path, &SMALL);
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
        let snapshots = rt.block_on(instance.list_snapshots()).expect("list works");
        b.iter(|| {
            rt.block_on(instance.world_mut(&mut world).rollback(
                snapshots[0].id,
                sekai_app::RollbackOptions::default(),
                Scope::World,
                |_| {},
            ))
            .expect("rollback works");
        });
        std::fs::remove_dir_all(&root).ok();
    });

    c.bench_function("rollback/corpus", |b| {
        let world_path = corpus_world();
        let mut world = HostWorldTree::new(&world_path);
        let root = world_path.parent().unwrap().to_path_buf();
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
        let snapshots = rt.block_on(instance.list_snapshots()).expect("list works");
        b.iter(|| {
            rt.block_on(instance.world_mut(&mut world).rollback(
                snapshots[0].id,
                sekai_app::RollbackOptions::default(),
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
