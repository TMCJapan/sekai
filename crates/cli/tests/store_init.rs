//! Commands other than `backup` must never create or initialize the store:
//! a missing store fails loudly and leaves nothing behind (issue #80).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use clap::Parser as _;
use sekai_cli::cli::Cli;

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sekai-cli-{name}-{}-{}",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn commands_on_a_missing_store_fail_without_creating_it() {
    let root = tempdir("missing-store");
    let store = root.join("store");
    let world = root.join("world");
    std::fs::create_dir_all(&world).unwrap();
    let store = store.to_str().expect("temporary store path is UTF-8");
    let world = world.to_str().expect("temporary world path is UTF-8");

    for args in [
        vec!["sekai", "--store", store, "list"],
        vec!["sekai", "--store", store, "tag", "list"],
        vec!["sekai", "--store", store, "gc"],
        vec!["sekai", "--store", store, "prune", "--keep-last", "1"],
        vec!["sekai", "--store", store, "status", world],
        vec!["sekai", "--store", store, "diff"],
        vec!["sekai", "--store", store, "rollback", world, "1"],
        vec!["sekai", "--store", store, "export", "1", "out"],
    ] {
        let cli = Cli::try_parse_from(&args).expect("valid invocation");
        let err = sekai_cli::commands::run(&cli)
            .await
            .expect_err("a missing store must fail");
        assert!(
            err.chain()
                .any(|cause| cause.to_string().contains("no store at")),
            "{args:?} failed for the wrong reason: {err:?}"
        );
        assert!(
            !root.join("store").exists(),
            "{args:?} must not create the store"
        );
    }

    cleanup(&root);
}

fn cleanup(dir: impl AsRef<Path>) {
    let _ = std::fs::remove_dir_all(dir);
}
