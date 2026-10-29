//! Integration benchmarks for checkout flows.

#[path = "common.rs"]
mod common;

use common::{SMALL, corpus_world, generate, options, runtime, tempdir};
use criterion::{Criterion, criterion_group, criterion_main};
use sekai_app::{HostWorldTree, Scope};

fn benches(c: &mut Criterion) {
    let rt = runtime();

    c.bench_function("checkout/small", |b| {
        let root = tempdir("checkout-small");
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
        b.iter(|| {
            rt.block_on(instance.world_mut(&mut world).checkout(
                commits[0].id,
                sekai_app::CheckoutOptions::default(),
                Scope::World,
                |_| {},
            ))
            .expect("checkout works");
        });
    });

    c.bench_function("checkout/corpus", |b| {
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
        b.iter(|| {
            rt.block_on(instance.world_mut(&mut world).checkout(
                commits[0].id,
                sekai_app::CheckoutOptions::default(),
                Scope::World,
                |_| {},
            ))
            .expect("checkout works");
        });
    });
}

criterion_group!(all_benches, benches);
criterion_main!(all_benches);
