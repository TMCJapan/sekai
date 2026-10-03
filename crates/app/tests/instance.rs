//! Store lifecycle: only `init` materializes a store, while `open` must fail
//! loudly on a root that was never initialized (issue #80).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sekai-app-{name}-{}-{}",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn cleanup(dir: impl AsRef<Path>) {
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn open_of_a_missing_store_fails_and_creates_nothing() {
    let root = tempdir("open-missing");
    let store = root.join("store");
    let url = store.to_string_lossy().into_owned();

    let Err(err) = sekai_app::SekaiInstance::open(&url).await else {
        panic!("open of a missing store must fail");
    };
    assert!(
        err.to_string().contains("no store at"),
        "unexpected error: {err}"
    );
    assert!(!store.exists(), "a failed open must not create anything");

    sekai_app::SekaiInstance::init(&url).await.unwrap();
    assert!(store.join("meta.sqlite").is_file());
    assert!(store.join("blobs").is_dir());
    sekai_app::SekaiInstance::open(&url).await.unwrap();
    cleanup(&root);
}

#[tokio::test]
async fn init_is_idempotent_on_an_existing_store() {
    let root = tempdir("init-twice");
    let store = root.join("store");
    let url = store.to_string_lossy().into_owned();

    sekai_app::SekaiInstance::init(&url).await.unwrap();
    sekai_app::SekaiInstance::init(&url).await.unwrap();
    assert!(store.join("meta.sqlite").is_file());
    cleanup(&root);
}
