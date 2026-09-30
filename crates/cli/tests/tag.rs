//! CLI-level tag argument validation must precede store creation.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use clap::Parser as _;
use sekai_cli::cli::Cli;

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sekai-cli-tag-{name}-{}-{}",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn run_tag(store: impl AsRef<Path>, args: &[&str]) -> anyhow::Result<()> {
    let store = store
        .as_ref()
        .to_str()
        .expect("temporary store path is UTF-8");
    let mut argv = vec!["sekai", "--store", store, "tag"];
    argv.extend_from_slice(args);
    let cli = Cli::try_parse_from(argv).expect("tag arguments parse");
    sekai_cli::commands::run(&cli).await
}

#[tokio::test]
async fn invalid_tag_arguments_do_not_create_the_store() {
    let root = tempdir("invalid-args");
    let store = root.join("store");

    // Name without a snapshot is a validation error, not an empty store.
    let err = run_tag(&store, &["abcde"]).await.unwrap_err();
    assert!(err.to_string().contains("tag creation needs both"));
    assert!(!store.exists(), "`tag abcde` must not create the store");

    // Deleting without a tag name is likewise rejected up front.
    let err = run_tag(&store, &["--delete"]).await.unwrap_err();
    assert!(err.to_string().contains("tag name is required"));
    assert!(!store.exists(), "`tag --delete` must not create the store");

    let _ = std::fs::remove_dir_all(&root);
}
