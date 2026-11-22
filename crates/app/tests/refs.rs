//! End-to-end ref flows over local stores.
//!
//! Requires the `backend-local` feature.

#![cfg(feature = "backend-local")]

mod fixtures;
mod support;

use fixtures::chunk_nbt;

use sekai_app::{HeadState, HostWorldTree, RefName};
use sekai_storage::{MetaStore as _, SekaiStore as _, SekaiStoreOpener as _};
use sekai_storage_local::LocalStore;
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
    instance: &mut sekai_app::SekaiInstance<S>,
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

/// The HEAD of the store's main worktree.
async fn main_head(instance: &sekai_app::SekaiInstance<LocalStore>) -> HeadState {
    let meta = instance.store().meta();
    let main = sekai_storage::refs::main_worktree(meta)
        .await
        .unwrap()
        .expect("an initialized store has a main worktree");
    meta.head(&main).await.unwrap().unwrap()
}

#[tokio::test]
async fn a_fresh_store_has_an_unborn_head() {
    let root = tempdir();
    let store = root.path().join("store").to_string_lossy().into_owned();
    let instance = support::init_store(&store).await.unwrap();

    assert_eq!(
        main_head(&instance).await,
        HeadState::Unborn {
            branch: RefName::default_branch()
        }
    );
    assert_eq!(instance.current_commit_id().await.unwrap(), None);
}

#[tokio::test]
async fn a_commit_lands_on_head() {
    let root = tempdir();
    let world_path = root.path().join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.path().join("store").to_string_lossy().into_owned();
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, chunk_nbt("marker-1"))],
    );

    let mut instance = support::init_store(&store).await.unwrap();
    let first = commit(&mut instance, &mut world).await;

    // HEAD's branch moved onto the new commit.
    assert_eq!(
        main_head(&instance).await,
        HeadState::Attached {
            branch: RefName::default_branch(),
            commit: first.commit,
        }
    );
    assert_eq!(
        instance.current_commit_id().await.unwrap(),
        Some(first.commit)
    );

    // A second commit continues it.
    let second = commit(&mut instance, &mut world).await;
    assert_eq!(
        instance.commit_parents(second.commit).await.unwrap(),
        vec![first.commit]
    );
}

#[tokio::test]
async fn a_store_without_a_main_worktree_has_no_head() {
    let root = tempdir();
    let world_path = root.path().join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, chunk_nbt("marker-1"))],
    );
    let store = LocalStore::init(root.path().join("store")).await.unwrap();
    let mut instance = sekai_app::SekaiInstance::new(store);

    assert!(matches!(
        instance.current_commit_id().await,
        Err(sekai_app::AppError::MissingMainWorktree)
    ));
    let err = instance
        .world_mut(&mut world)
        .commit(
            sekai_app::CommitOptions::default(),
            sekai_app::Scope::World,
            &(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, sekai_app::AppError::MissingMainWorktree),
        "unexpected error: {err}"
    );
}
