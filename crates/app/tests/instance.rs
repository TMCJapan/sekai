//! Store lifecycle: only `init` materializes a store, while `open` must fail
//! loudly on a root that was never initialized (issue #80).
//!
//! Requires the `backend-local` feature.

#![cfg(feature = "backend-local")]

use sekai_storage::{MetaStore as _, SekaiStore as _};
use sekai_storage_local::LocalStore;

/// A temp dir whose name starts with the calling test's name (tempfile
/// appends a random suffix), removed when its guard is dropped at the end
/// of the test, panics included.
fn tempdir(name: &str) -> (std::path::PathBuf, tempfile::TempDir) {
    let guard = tempfile::Builder::new()
        .prefix(&format!("sekai-app-{name}-"))
        .tempdir()
        .unwrap();
    (guard.path().to_path_buf(), guard)
}

#[tokio::test]
async fn open_of_a_missing_store_fails_and_creates_nothing() {
    let (root, _guard) = tempdir("open-missing");
    let store = root.join("store");
    let url = store.to_string_lossy().into_owned();

    let Err(err) = sekai_app::SekaiInstance::<LocalStore>::open(&url).await else {
        panic!("open of a missing store must fail");
    };
    assert!(
        err.to_string().contains("no store at"),
        "unexpected error: {err}"
    );
    assert!(!store.exists(), "a failed open must not create anything");

    sekai_app::SekaiInstance::<LocalStore>::init(&url, root.join("world"))
        .await
        .unwrap();
    assert!(store.join("version").is_file());
    assert!(store.join("meta.sqlite").is_file());
    assert!(store.join("blobs/conf").is_file());
    assert!(store.join("cache/conf").is_file());
    sekai_app::SekaiInstance::<LocalStore>::open(&url)
        .await
        .unwrap();
}

#[tokio::test]
async fn init_registers_the_world_as_the_main_worktree() {
    let (root, _guard) = tempdir("init-main");
    let world = root.join("world");
    let instance = sekai_app::SekaiInstance::<LocalStore>::init(root.join("store"), &world)
        .await
        .unwrap();
    let mut worktrees = Vec::new();
    instance
        .store()
        .meta()
        .visit_worktrees(|worktree| {
            worktrees.push(worktree.clone());
            true
        })
        .await
        .unwrap();
    assert_eq!(worktrees.len(), 1);
    assert!(worktrees[0].main);
    assert_eq!(worktrees[0].path, world.to_str().unwrap());
    assert_eq!(
        worktrees[0].head,
        sekai_app::Head::Branch(sekai_app::RefName::default_branch())
    );
}

#[tokio::test]
async fn init_is_idempotent_from_the_same_world() {
    let (root, _guard) = tempdir("init-twice");
    let store = root.join("store");
    let url = store.to_string_lossy().into_owned();
    let world = root.join("world");

    let instance = sekai_app::SekaiInstance::<LocalStore>::init(&url, &world)
        .await
        .unwrap();
    drop(instance);
    let instance = sekai_app::SekaiInstance::<LocalStore>::init(&url, &world)
        .await
        .unwrap();
    assert!(store.join("meta.sqlite").is_file());
    drop(instance);

    let Err(err) = sekai_app::SekaiInstance::<LocalStore>::init(&url, root.join("other")).await
    else {
        panic!("init from another world must fail");
    };
    assert!(
        matches!(&err, sekai_app::AppError::StoreHasOtherWorld { main, .. }
            if main == world.to_str().unwrap()),
        "unexpected error: {err}"
    );
}

/// The backend-generic entry points take the backend's own path type, so a
/// `PathBuf` opens a local store without a lossy string round-trip.
#[tokio::test]
async fn open_and_init_take_path_like_locations() {
    use sekai_app::SekaiInstance;
    use sekai_storage_local::LocalStore;

    let (root, _guard) = tempdir("with-path");
    let store = root.join("store");

    let Err(err) = SekaiInstance::<LocalStore>::open(&store).await else {
        panic!("open of a missing store must fail");
    };
    assert!(
        err.to_string().contains("no store at"),
        "unexpected error: {err}"
    );
    assert!(!store.exists(), "a failed open must not create anything");

    SekaiInstance::<LocalStore>::init(store.clone(), root.join("world"))
        .await
        .unwrap();
    assert!(store.join("meta.sqlite").is_file());
    SekaiInstance::<LocalStore>::open(store.as_path())
        .await
        .unwrap();
}
