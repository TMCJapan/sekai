//! End-to-end ref flows over SQLite stores.
//!
//! Requires the `backend-sqlite` feature.

#![cfg(feature = "backend-sqlite")]

use sekai_app::{DEFAULT_REF_NAME, HostWorldTree, RefName};
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
            |_| {},
        )
        .await
        .unwrap()
        .0
}

#[tokio::test]
async fn a_fresh_store_starts_with_one_active_ref() {
    let root = tempdir();
    let store = root.path().join("store").to_string_lossy().into_owned();
    let instance = sekai_app::SekaiInstance::init(&store).await.unwrap();

    let refs = instance.list_refs().await.unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name.as_str(), DEFAULT_REF_NAME);
    assert!(refs[0].active);
    // Unborn: the store has no commit yet.
    assert!(refs[0].commit.is_none());
}

#[tokio::test]
async fn a_commit_hangs_from_the_active_ref() {
    let root = tempdir();
    let world_path = root.path().join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.path().join("store").to_string_lossy().into_owned();
    write_region(world_path.join("region/r.0.0.mca"), &[(0, 0, vec![3, 1])]);

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    let first = commit(&mut instance, &mut world).await;

    // The active ref moved onto the new commit.
    let active = instance
        .active_ref()
        .await
        .unwrap()
        .expect("a store always has an active ref");
    assert_eq!(active.name.as_str(), DEFAULT_REF_NAME);
    assert_eq!(active.commit, Some(first.commit));
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
async fn switching_refs_forks_the_history() {
    let root = tempdir();
    let world_path = root.path().join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.path().join("store").to_string_lossy().into_owned();
    write_region(world_path.join("region/r.0.0.mca"), &[(0, 0, vec![3, 1])]);

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    let first = commit(&mut instance, &mut world).await;

    // A second ref starts at the active ref's target.
    let feature = RefName::parse("feature").unwrap();
    let record = instance
        .create_ref(&feature, Some(first.commit), false)
        .await
        .unwrap();
    assert_eq!(record.commit, Some(first.commit));
    assert!(!record.active);

    // Moving an existing ref needs `force`.
    assert!(
        instance
            .create_ref(&feature, Some(first.commit), false)
            .await
            .is_err()
    );

    // The active ref decides where the next commit lands.
    instance.activate_ref(&feature).await.unwrap();
    let on_feature = commit(&mut instance, &mut world).await;
    assert_eq!(
        instance.commit_parents(on_feature.commit).await.unwrap(),
        vec![first.commit]
    );

    // Back on `main`, the next commit forks instead of continuing.
    instance
        .activate_ref(&RefName::parse(DEFAULT_REF_NAME).unwrap())
        .await
        .unwrap();
    let on_main = commit(&mut instance, &mut world).await;
    assert_eq!(
        instance.commit_parents(on_main.commit).await.unwrap(),
        vec![first.commit]
    );
    assert_ne!(on_main.commit, on_feature.commit);

    // Ref names resolve like commit references.
    assert_eq!(
        instance.resolve_commit_ref("feature").await.unwrap(),
        on_feature.commit
    );
}

#[tokio::test]
async fn the_active_ref_is_protected() {
    let root = tempdir();
    let store = root.path().join("store").to_string_lossy().into_owned();
    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    let main = RefName::parse(DEFAULT_REF_NAME).unwrap();

    // The store's active ref never goes away.
    assert!(instance.delete_ref(&main).await.is_err());

    let other = RefName::parse("other").unwrap();
    instance.create_ref(&other, None, false).await.unwrap();
    assert!(instance.delete_ref(&other).await.unwrap());
    assert!(!instance.delete_ref(&other).await.unwrap());
    assert_eq!(instance.list_refs().await.unwrap().len(), 1);
}

#[tokio::test]
async fn forcing_a_move_keeps_the_active_flag() {
    let root = tempdir();
    let world_path = root.path().join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.path().join("store").to_string_lossy().into_owned();
    write_region(world_path.join("region/r.0.0.mca"), &[(0, 0, vec![3, 1])]);

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    let first = commit(&mut instance, &mut world).await;
    let _second = commit(&mut instance, &mut world).await;
    let main = RefName::parse(DEFAULT_REF_NAME).unwrap();

    // Moving the active ref back moves the flag with it: it stays the
    // ref the next commit hangs from.
    let record = instance
        .create_ref(&main, Some(first.commit), true)
        .await
        .unwrap();
    assert!(record.active);
    assert_eq!(record.commit, Some(first.commit));
    let created_at_ms = record.created_at_ms;
    assert_eq!(
        instance.current_commit_id().await.unwrap(),
        Some(first.commit)
    );

    // A forced move to unborn keeps the flag and creation time too.
    let record = instance.create_ref(&main, None, true).await.unwrap();
    assert!(record.active);
    assert!(record.commit.is_none());
    assert_eq!(record.created_at_ms, created_at_ms);
    assert_eq!(instance.list_refs().await.unwrap().len(), 1);
    let active = instance.active_ref().await.unwrap().unwrap();
    assert_eq!(active.name, main);
    assert!(active.commit.is_none());
}

#[tokio::test]
async fn ref_writes_report_a_missing_target() {
    let root = tempdir();
    let store = root.path().join("store").to_string_lossy().into_owned();
    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    let feature = RefName::parse("feature").unwrap();

    // A target no commit carries is refused before the ref exists.
    let err = instance
        .create_ref(&feature, Some(sekai_app::CommitId([99; 32])), false)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            sekai_app::AppError::CommitRef(sekai_core::ResolveError::UnknownCommit {
                id: sekai_app::CommitId(id)
            }) if id == [99; 32]
        ),
        "{err}"
    );
    assert_eq!(instance.list_refs().await.unwrap().len(), 1);

    // Activating a name no ref carries is refused, and the active ref
    // stays where it was.
    let err = instance.activate_ref(&feature).await.unwrap_err();
    assert!(
        matches!(err, sekai_app::AppError::UnknownRef { .. }),
        "{err}"
    );
    let active = instance
        .active_ref()
        .await
        .unwrap()
        .expect("a store always has an active ref");
    assert_eq!(active.name.as_str(), DEFAULT_REF_NAME);
}
