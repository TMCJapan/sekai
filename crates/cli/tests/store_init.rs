//! Commands other than `commit` must never create or initialize the store:
//! a missing store fails loudly and leaves nothing behind (issue #80).

use clap::Parser as _;
use sekai_cli::cli::Cli;

const HASH: &str = "0101010101010101010101010101010101010101010101010101010101010101";

/// A temp dir named after the calling test, removed when its guard is
/// dropped at the end of the test, panics included.
fn tempdir(name: &str) -> (std::path::PathBuf, tempfile::TempDir) {
    let guard = tempfile::Builder::new()
        .prefix(&format!("sekai-cli-{name}-"))
        .tempdir()
        .unwrap();
    (guard.path().to_path_buf(), guard)
}

#[tokio::test]
async fn commands_on_a_missing_store_fail_without_creating_it() {
    let (root, _guard) = tempdir("missing-store");
    let store = root.join("store");
    let world = root.join("world");
    std::fs::create_dir_all(&world).unwrap();
    let store = store.to_str().expect("temporary store path is UTF-8");
    let world = world.to_str().expect("temporary world path is UTF-8");

    for args in [
        vec!["sekai", "--store", store, "list"],
        vec!["sekai", "--store", store, "gc"],
        vec!["sekai", "--store", store, "prune", "--keep-last", "1"],
        vec!["sekai", "--store", store, "status", world],
        vec!["sekai", "--store", store, "diff"],
        vec!["sekai", "--store", store, "checkout", world, HASH],
        vec!["sekai", "--store", store, "clone", HASH, "out"],
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
}
