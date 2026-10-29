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
        let commits = rt.block_on(instance.list_commits()).expect("list works");
        let coords = rt
            .block_on(instance.commit_chunk_coords(commits[0].id))
            .expect("coords work");
        b.iter(|| {
            rt.block_on(instance.diff_chunks(commits[0].id, commits[0].id, &coords, None, |_| {}))
                .expect("diff works");
        });
    });

    c.bench_function("diff/corpus-all", |b| {
        let corpus = corpus_world();
        let world_path = corpus.path().join("world");
        let mut world = HostWorldTree::new(&world_path).expect("world resolves");
        let store = world_path
            .parent()
            .unwrap()
            .join("store")
            .to_string_lossy()
            .into_owned();
        let mut instance = rt
            .block_on(sekai_app::SekaiInstance::init(&store))
            .expect("open works");
        rt.block_on(
            instance
                .world_mut(&mut world)
                .commit(options(), Scope::World, |_| {}),
        )
        .expect("commit works");
        let commits = rt.block_on(instance.list_commits()).expect("list works");
        let coords = rt
            .block_on(instance.commit_chunk_coords(commits[0].id))
            .expect("coords work");
        b.iter(|| {
            rt.block_on(instance.diff_chunks(commits[0].id, commits[0].id, &coords, None, |_| {}))
                .expect("diff works");
        });
    });
}

criterion_group!(all_benches, benches);
criterion_main!(all_benches);
