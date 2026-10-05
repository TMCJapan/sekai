//! Integration benchmarks for garbage collection and scanning.

#[path = "common.rs"]
mod common;

use common::{SMALL, corpus_world, generate, options, runtime, tempdir};
use criterion::{Criterion, criterion_group, criterion_main};
use sekai_app::{HostWorldTree, Scope};

fn benches(c: &mut Criterion) {
    let rt = runtime();

    c.bench_function("gc-plan/small", |b| {
        let root = tempdir("gc-small");
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
        // Plan is read-only and repeatable; apply (unlink syscalls) is
        // covered by unit tests.
        b.iter(|| {
            rt.block_on(instance.gc_plan()).expect("gc plan works");
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
