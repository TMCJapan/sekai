//! End-to-end ref flows over local stores.

mod fixtures;

use fixtures::chunk_nbt;

use sekai_app::DEFAULT_REF_NAME;
use sekai_world_local::HostWorldTree;
use std::path::Path;

/// A private temp dir, removed on drop. Nothing is kept, so a panicking
/// test leaves nothing behind either.
fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("sekai-ref-")
        .tempdir()
        .unwrap()
}

fn write_region(path: impl AsRef<Path>, chunks: &[(i32, i32, Vec<u8>)]) {
    let name = path.as_ref().file_name().unwrap().to_str().unwrap();
    let (rx, rz) = sekai_anvil::parse_region_name(name).unwrap();
    let mut builder = sekai_anvil::RegionBuilder::new(rx, rz, 0).unwrap();
    for (x, z, payload) in chunks {
        builder.stage_chunk(*x, *z, payload).unwrap();
    }
    let image = builder.image().unwrap();
    std::fs::create_dir_all(path.as_ref().parent().unwrap()).unwrap();
    std::fs::write(path, image).unwrap();
}

async fn commit<S: sekai_app::SekaiStore>(
    instance: &mut sekai_app::SekaiInstance<S, impl sekai_app::ThreadRunner, impl sekai_app::Clock>,
    world_tree: &mut HostWorldTree,
) -> sekai_app::CommitReport {
    instance
        .world_mut(world_tree)
        .commit(
            sekai_app::CommitOptions::default(),
            sekai_app::Scope::World,
            &(),
        )
        .await
        .unwrap()
        .0
}

#[tokio::test]
async fn a_fresh_store_starts_with_one_active_ref() {
    let root = tempdir();
    let store = root.path().join("store").to_string_lossy().into_owned();
    let instance = fixtures::init(&store).await.unwrap();

    let active = instance
        .active_ref()
        .await
        .unwrap()
        .expect("a store always has an active ref");
    assert_eq!(active.name.name.as_str(), DEFAULT_REF_NAME);
    assert!(active.active);
    // Unborn: the store has no commit yet.
    assert!(active.commit.is_none());
    assert_eq!(instance.current_commit_id().await.unwrap(), None);
    // Listed although unborn: the row exists.
    assert_eq!(instance.list_branches().await.unwrap(), vec![active]);
}

#[tokio::test]
async fn a_commit_hangs_from_the_active_ref() {
    let root = tempdir();
    let world_path = root.path().join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.path().join("store").to_string_lossy().into_owned();
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, chunk_nbt("marker-1"))],
    );

    let mut instance = fixtures::init(&store).await.unwrap();
    let first = commit(&mut instance, &mut world).await;

    // The active ref moved onto the new commit.
    let active = instance
        .active_ref()
        .await
        .unwrap()
        .expect("a store always has an active ref");
    assert_eq!(active.name.name.as_str(), DEFAULT_REF_NAME);
    assert_eq!(active.commit, Some(first.commit));
    assert_eq!(
        instance.current_commit_id().await.unwrap(),
        Some(first.commit)
    );

    assert_eq!(instance.list_branches().await.unwrap(), vec![active]);

    // A second commit continues it.
    let second = commit(&mut instance, &mut world).await;
    assert_eq!(
        instance.commit_parents(second.commit).await.unwrap(),
        vec![first.commit]
    );
}
