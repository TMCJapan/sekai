//! Only `init` creates the store: every other command (including `commit`)
//! must fail loudly on a missing store and leave nothing behind (issue #80).

use clap::Parser as _;
use sekai_cli::cli::Cli;

const HASH: &str = "0101010101010101010101010101010101010101010101010101010101010101";

/// A temp dir whose name starts with the calling test's name (tempfile
/// appends a random suffix), removed when its guard is dropped at the end
/// of the test, panics included.
fn tempdir(name: &str) -> (std::path::PathBuf, tempfile::TempDir) {
    let guard = tempfile::Builder::new()
        .prefix(&format!("sekai-cli-{name}-"))
        .tempdir()
        .unwrap();
    (guard.path().to_path_buf(), guard)
}

async fn run(args: &[&str]) -> anyhow::Result<()> {
    let cli = Cli::try_parse_from(args).expect("valid invocation");
    sekai_cli::commands::run(&cli).await
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
        vec!["sekai", "--store", store, "commit", world],
        vec!["sekai", "--store", store, "log"],
        vec!["sekai", "--store", store, "gc"],
        vec!["sekai", "--store", store, "prune", "--keep-last", "1"],
        vec!["sekai", "--store", store, "status", world],
        vec!["sekai", "--store", store, "diff"],
        vec!["sekai", "--store", store, "checkout", world, HASH],
        vec!["sekai", "--store", store, "ref", "list"],
        vec!["sekai", "--store", store, "scope", "list"],
        vec!["sekai", "--store", store, "scope", "delete", "spawn"],
        vec![
            "sekai", "--store", store, "scope", "create", "spawn", "--in", "ow:0,0",
        ],
    ] {
        let err = run(&args).await.expect_err("a missing store must fail");
        assert!(
            err.chain()
                .any(|cause| cause.to_string().contains("no store at")),
            "{args:?} failed for the wrong reason: {err:?}"
        );
        assert!(
            err.chain()
                .any(|cause| cause.to_string().contains("sekai init")),
            "{args:?} must point at 'sekai init': {err:?}"
        );
        assert!(
            !root.join("store").exists(),
            "{args:?} must not create the store"
        );
    }
}

#[tokio::test]
async fn init_creates_the_store_every_other_command_opens() {
    let (root, _guard) = tempdir("init-flow");
    let store = root.join("store");
    let world = root.join("world");
    std::fs::create_dir_all(&world).unwrap();
    let store = store.to_str().expect("temporary store path is UTF-8");
    let world = world.to_str().expect("temporary world path is UTF-8");

    assert!(!sekai_app::local_store_exists(store));
    run(&["sekai", "--store", store, "init"])
        .await
        .expect("init creates the store");
    assert!(sekai_app::local_store_exists(store));
    assert!(root.join("store/meta.sqlite").is_file());
    assert!(root.join("store/blobs/conf").is_file());

    // Re-initializing an existing store is idempotent, like `git init`.
    run(&["sekai", "--store", store, "init", "--json"])
        .await
        .expect("init is idempotent");

    for args in [
        vec!["sekai", "--store", store, "commit", world],
        vec!["sekai", "--store", store, "log"],
        vec!["sekai", "--store", store, "ref", "list"],
    ] {
        run(&args)
            .await
            .unwrap_or_else(|err| panic!("{args:?} failed: {err:?}"));
    }
}
