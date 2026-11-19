//! Store lifecycle: only `init_world` materializes a store, while
//! `open_location` must fail loudly on a store that was never initialized
//! (issue #80).
//!
//! Requires the `backend-local` feature.

#![cfg(feature = "backend-local")]

use std::path::Path;

use sekai_app::{Location, SekaiInstance};

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

/// A world `init_world` recognizes: an empty region file is the empty
/// region.
fn world_at(path: impl AsRef<Path>) {
    let region = path.as_ref().join("region");
    std::fs::create_dir_all(&region).unwrap();
    std::fs::write(region.join("r.0.0.mca"), []).unwrap();
}

#[tokio::test]
async fn open_of_a_missing_store_fails_and_creates_nothing() {
    let (root, _guard) = tempdir("open-missing");
    let store = root.join("store");
    let outside = Location {
        store: store.clone(),
        world: None,
    };

    let Err(err) = SekaiInstance::open_location(&outside).await else {
        panic!("open of a missing store must fail");
    };
    assert!(
        err.to_string().contains("no store at"),
        "unexpected error: {err}"
    );
    assert!(!store.exists(), "a failed open must not create anything");

    world_at(root.join("world"));
    SekaiInstance::init_world(root.join("world"), Some(&store))
        .await
        .unwrap();
    assert!(store.join("meta.sqlite").is_file());
    assert!(store.join("blobs/conf").is_file());
    assert!(store.join("cache/conf").is_file());
    let opened = SekaiInstance::open_location(&outside).await.unwrap();
    assert_eq!(opened.worktree(), None);
}

#[tokio::test]
async fn init_is_idempotent_on_an_existing_store() {
    let (root, _guard) = tempdir("init-twice");
    let store = root.join("store");
    world_at(root.join("world"));

    let first = SekaiInstance::init_world(root.join("world"), Some(&store))
        .await
        .unwrap();
    assert!(first.created);
    drop(first);
    let second = SekaiInstance::init_world(root.join("world"), Some(&store))
        .await
        .unwrap();
    assert!(!second.created);
    assert!(store.join("meta.sqlite").is_file());
}
