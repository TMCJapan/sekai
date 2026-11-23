//! Store lifecycle: only `init` materializes a store, while `open` must fail
//! loudly on a root that was never initialized (issue #80).
//!
//! Requires the `backend-local` feature.

#![cfg(feature = "backend-local")]

use sekai_storage_local::LocalStore;

mod fixtures;

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

    sekai_app::SekaiInstance::<LocalStore>::init(&url)
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
async fn init_is_idempotent_on_an_existing_store() {
    let (root, _guard) = tempdir("init-twice");
    let store = root.join("store");
    let url = store.to_string_lossy().into_owned();

    sekai_app::SekaiInstance::<LocalStore>::init(&url)
        .await
        .unwrap();
    sekai_app::SekaiInstance::<LocalStore>::init(&url)
        .await
        .unwrap();
    assert!(store.join("meta.sqlite").is_file());
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

    SekaiInstance::<LocalStore>::init(store.clone())
        .await
        .unwrap();
    assert!(store.join("meta.sqlite").is_file());
    SekaiInstance::<LocalStore>::open(store.as_path())
        .await
        .unwrap();
}

/// The whole write/read pipeline runs over any `ThreadRunner`, not just
/// the host pool: the inline `Sequential` runner (the `no_std` baseline)
/// commits and checks out the same world through the same flows.
#[tokio::test]
async fn the_flows_run_over_a_custom_runner() {
    use sekai_app::{
        CheckoutOptions, CommitOptions, Scope, SekaiInstance, Sequential, ThreadRunner,
    };

    let (root, _guard) = tempdir("custom-runner");
    let world_path = root.join("world");
    let region = world_path.join("region/r.0.0.mca");
    let mut builder = sekai_anvil::RegionBuilder::new(0, 0, 0).unwrap();
    builder
        .stage_chunk(0, 0, &fixtures::chunk_nbt("sequential"))
        .unwrap();
    std::fs::create_dir_all(region.parent().unwrap()).unwrap();
    std::fs::write(&region, builder.image().unwrap()).unwrap();
    let mut world = sekai_app::HostWorldTree::new(&world_path).unwrap();

    let store = root.join("store").to_string_lossy().into_owned();
    let mut instance =
        SekaiInstance::<LocalStore, Sequential>::init_with(&store, Sequential::new(0))
            .await
            .unwrap();
    let (report, _) = instance
        .world_mut(&mut world)
        .commit(CommitOptions::default(), Scope::World, &())
        .await
        .unwrap();
    assert_eq!(report.chunks, 1);

    std::fs::remove_file(&region).unwrap();
    let (checked_out, _) = instance
        .world_mut(&mut world)
        .checkout(report.commit, CheckoutOptions::default(), Scope::World, &())
        .await
        .unwrap();
    assert_eq!(checked_out.files_written, 1);
    assert_eq!(checked_out.chunks_restored, 1);
    assert!(region.exists());
}
