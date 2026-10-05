//! Integration benchmarks for chunk diff flows.

#[path = "common.rs"]
mod common;

use common::{SMALL, corpus_world, generate, options, runtime, tempdir};
use criterion::{Criterion, criterion_group, criterion_main};
use sekai_app::{HostWorldTree, Scope};

fn benches(c: &mut Criterion) {
    let rt = runtime();

    c.bench_function("diff/small-all", |b| {
        let root = tempdir("diff-small");
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
        let coords = rt
            .block_on(instance.snapshot_chunk_coords(snapshots[0].id))
            .expect("coords work");
        b.iter(|| {
            rt.block_on(instance.diff_chunks(
                snapshots[0].id,
                snapshots[0].id,
                &coords,
                None,
                |_| {},
            ))
            .expect("diff works");
        });
        std::fs::remove_dir_all(&root).ok();
    });

    c.bench_function("diff/corpus-all", |b| {
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
        let coords = rt
            .block_on(instance.snapshot_chunk_coords(snapshots[0].id))
            .expect("coords work");
        b.iter(|| {
            rt.block_on(instance.diff_chunks(
                snapshots[0].id,
                snapshots[0].id,
                &coords,
                None,
                |_| {},
            ))
            .expect("diff works");
        });
        std::fs::remove_dir_all(&root).ok();
    });
}

criterion_group!(all_benches, benches);
criterion_main!(all_benches);
