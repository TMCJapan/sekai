//! Integration benchmarks for garbage collection.

#[path = "common.rs"]
mod common;

use common::{SMALL, generate, options, runtime, tempdir};
use criterion::{Criterion, criterion_group, criterion_main};
use sekai_app::{HostWorldTree, Scope};

fn benches(c: &mut Criterion) {
    let rt = runtime();

    c.bench_function("gc-plan/small", |b| {
        let root = tempdir("gc-small");
        let world_path = root.join("world");
        let mut world = HostWorldTree::new(&world_path).expect("world resolves");
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
}

criterion_group!(all_benches, benches);
criterion_main!(all_benches);
