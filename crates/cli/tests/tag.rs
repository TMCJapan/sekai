//! Malformed tag invocations must be refused by clap at parse time, so
//! they can never reach `commands::run` and create the store as a side
//! effect (issue #80).

use std::path::PathBuf;
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

#[test]
fn invalid_tag_arguments_do_not_create_the_store() {
    let root = tempdir("invalid-args");
    let store = root.join("store");
    let store = store.to_str().expect("temporary store path is UTF-8");

    // Incomplete operations and the legacy flat surface are parse errors:
    // clap rejects them before any sekai code runs.
    for args in [
        vec!["sekai", "--store", store, "tag", "abcde"],
        vec!["sekai", "--store", store, "tag", "--delete"],
        vec!["sekai", "--store", store, "tag", "create", "abcde"],
        vec!["sekai", "--store", store, "tag", "delete"],
        vec!["sekai", "--store", store, "tag", "list", "abcde"],
    ] {
        assert!(Cli::try_parse_from(&args).is_err(), "{args:?} is refused");
    }
    assert!(
        !root.join("store").exists(),
        "rejected tag arguments must not create the store"
    );

    let _ = std::fs::remove_dir_all(&root);
}
