//! End-to-end ref flows over local stores.

mod fixtures;

use fixtures::chunk_nbt;

use sekai_app::{
    AppError, DEFAULT_REF_NAME, Head, HeadState, ListedBranch, Ref, RefName, Revision,
};
use sekai_storage::{MetaStore as _, SekaiStore as _, StorageError};
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

fn main_branch() -> RefName {
    RefName::parse(DEFAULT_REF_NAME).unwrap()
}

#[tokio::test]
async fn a_fresh_store_starts_unborn_on_main() {
    let root = tempdir();
    let store = root.path().join("store").to_string_lossy().into_owned();
    let instance = fixtures::init(&store).await.unwrap();

    assert_eq!(
        instance.head().await.unwrap(),
        HeadState::Unborn {
            branch: main_branch()
        }
    );
    assert_eq!(instance.current_commit_id().await.unwrap(), None);
    // Listed although unborn.
    assert_eq!(
        instance.list_branches().await.unwrap(),
        [ListedBranch {
            name: main_branch(),
            commit: None,
            head: true
        }]
    );
}

#[tokio::test]
async fn a_commit_lands_on_heads_branch() {
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

    assert_eq!(
        instance.head().await.unwrap(),
        HeadState::Attached {
            branch: main_branch(),
            commit: first.commit
        }
    );
    assert_eq!(
        instance.current_commit_id().await.unwrap(),
        Some(first.commit)
    );
    assert_eq!(
        instance.list_branches().await.unwrap(),
        [ListedBranch {
            name: main_branch(),
            commit: Some(first.commit),
            head: true
        }]
    );

    // A second commit continues it.
    let second = commit(&mut instance, &mut world).await;
    assert_eq!(
        instance.commit_parents(second.commit).await.unwrap(),
        vec![first.commit]
    );
}

#[tokio::test]
async fn each_worktree_commits_on_its_own_head() {
    let root = tempdir();
    let world_path = root.path().join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.path().join("store");
    let link = root.path().join("linked.sekai");
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, chunk_nbt("marker-1"))],
    );

    let first = {
        let mut instance = fixtures::init(&store).await.unwrap();
        let first = commit(&mut instance, &mut world).await.commit;
        let feature = Ref::branch(RefName::parse("feature").unwrap());
        instance
            .store_mut()
            .meta_mut()
            .create_ref(&feature, first, 0)
            .await
            .unwrap();
        instance
            .store()
            .add_worktree(&link, Head::Branch(feature.name))
            .await
            .unwrap();
        first
    };

    // Opened through its link, the linked worktree commits on `feature`.
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, chunk_nbt("marker-2"))],
    );
    let second = {
        let mut linked = fixtures::open(&link).await.unwrap();
        let second = commit(&mut linked, &mut world).await.commit;
        let listed: Vec<_> = linked
            .list_branches()
            .await
            .unwrap()
            .into_iter()
            .map(|listed| (listed.name.to_string(), listed.commit, listed.head))
            .collect();
        assert_eq!(
            listed,
            [
                ("feature".to_owned(), Some(second), true),
                ("main".to_owned(), Some(first), false),
            ]
        );
        second
    };
    assert_eq!(
        fixtures::open(&link)
            .await
            .unwrap()
            .commit_parents(second)
            .await
            .unwrap(),
        [first]
    );

    // The store's own worktree stayed on `main`.
    let own = fixtures::open(&store).await.unwrap();
    assert_eq!(own.current_commit_id().await.unwrap(), Some(first));
}

#[tokio::test]
async fn an_unregistered_location_has_no_head() {
    let root = tempdir();
    let store = root.path().join("store");
    drop(fixtures::init(&store).await.unwrap());
    // A store link written by hand, never registered as a worktree.
    let link = root.path().join("stray.sekai");
    std::fs::write(&link, format!("{}\n", store.display())).unwrap();

    let instance = fixtures::open(&link).await.unwrap();
    // Every way of reading HEAD fails alike.
    let errors = [
        instance.head().await.map(drop),
        instance.current_commit_id().await.map(drop),
        instance.resolve_revision(&Revision::head()).await.map(drop),
    ];
    for err in errors {
        assert!(matches!(
            err,
            Err(AppError::Storage(StorageError::UnknownWorktree { .. }))
        ));
    }
    // Commands that never read HEAD still work.
    assert_eq!(instance.list_commits().await.unwrap(), []);
}
