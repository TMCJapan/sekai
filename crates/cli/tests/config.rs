//! Store config driven through the CLI over a local store.

use clap::Parser as _;
use sekai_app::SekaiStoreOpener as _;
use sekai_cli::cli::Cli;
use sekai_storage_local::LocalStore;
use sekai_storage_local::config::{Config, ConfigKey};

/// A temp dir whose name starts with the calling test's name (tempfile
/// appends a random suffix), removed when its guard is dropped.
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
async fn config_set_get_unset_roundtrip() {
    let (root, _guard) = tempdir("config-flow");
    let store = root.join("store");
    let store = store.to_str().expect("temporary store path is UTF-8");

    run(&["sekai", "--store", store, "init"])
        .await
        .expect("init creates the store");

    // An unset key fails like git's exit 1; `--default` answers instead.
    let err = run(&["sekai", "--store", store, "config", "user.name"])
        .await
        .expect_err("an unset key has no value");
    assert!(
        err.chain()
            .any(|cause| cause.to_string() == "config key 'user.name' is not set")
    );
    run(&[
        "sekai",
        "--store",
        store,
        "config",
        "user.name",
        "--default",
        "fallback",
    ])
    .await
    .expect("--default answers an unset key");

    run(&["sekai", "--store", store, "config", "user.name", "foo"])
        .await
        .expect("config set");
    assert_eq!(
        LocalStore::open(store)
            .await
            .unwrap()
            .read_config()
            .unwrap()
            .get(ConfigKey::UserName),
        Some("foo")
    );
    assert_eq!(
        std::fs::read_to_string(root.join("store/config.toml")).unwrap(),
        "[user]\nname = \"foo\"\n"
    );

    // Reads accept the bare key, `--get`, and case-insensitive spellings.
    for args in [
        vec!["sekai", "--store", store, "config", "user.name"],
        vec!["sekai", "--store", store, "config", "--get", "user.name"],
        vec!["sekai", "--store", store, "config", "USER.NAME"],
        vec!["sekai", "--store", store, "config", "user.name", "--json"],
    ] {
        run(&args)
            .await
            .unwrap_or_else(|err| panic!("{args:?} failed: {err:?}"));
    }

    for args in [
        vec!["sekai", "--store", store, "config", "--list"],
        vec!["sekai", "--store", store, "config", "-l", "--name-only"],
        vec!["sekai", "--store", store, "config", "--list", "--json"],
    ] {
        run(&args)
            .await
            .unwrap_or_else(|err| panic!("{args:?} failed: {err:?}"));
    }

    // A second set replaces the value; the file holds one entry.
    run(&["sekai", "--store", store, "config", "user.name", "bar"])
        .await
        .expect("config set replaces");
    assert_eq!(
        LocalStore::open(store)
            .await
            .unwrap()
            .read_config()
            .unwrap()
            .get(ConfigKey::UserName),
        Some("bar")
    );

    run(&["sekai", "--store", store, "config", "user.name", "--unset"])
        .await
        .expect("config unset");
    assert_eq!(
        LocalStore::open(store)
            .await
            .unwrap()
            .read_config()
            .unwrap(),
        Config::default()
    );
    let err = run(&["sekai", "--store", store, "config", "user.name", "--unset"])
        .await
        .expect_err("unsetting an unset key fails");
    assert!(
        err.chain()
            .any(|cause| cause.to_string() == "config key 'user.name' is not set")
    );
}

#[tokio::test]
async fn config_opens_the_store_exclusively() {
    let (root, _guard) = tempdir("config-open");
    let store = root.join("store");
    let store = store.to_str().expect("temporary store path is UTF-8");

    run(&["sekai", "--store", store, "init"])
        .await
        .expect("init creates the store");

    // Config goes through the same exclusive open as every operation:
    // while another handle holds the sled session locks, it fails loudly.
    let opened = LocalStore::open(store).await.unwrap();
    let err = run(&["sekai", "--store", store, "config", "user.name", "foo"])
        .await
        .expect_err("a second opener must fail");
    assert!(
        err.chain()
            .any(|cause| cause.to_string().contains("open of")),
        "failed for the wrong reason: {err:?}"
    );
    drop(opened);

    // Once the handle is gone, config works again.
    run(&["sekai", "--store", store, "config", "user.name", "foo"])
        .await
        .expect("config set after the lock is released");
}

#[tokio::test]
async fn config_follows_a_store_link() {
    let (root, _guard) = tempdir("config-link");
    let store = root.join("store");
    let store = store.to_str().expect("temporary store path is UTF-8");
    run(&["sekai", "--store", store, "init"])
        .await
        .expect("init creates the store");

    let link = root.join("store.link");
    std::fs::write(&link, store).unwrap();
    let link = link.to_str().expect("temporary link path is UTF-8");
    run(&["sekai", "--store", link, "config", "user.name", "foo"])
        .await
        .expect("config set through a store link");
    assert!(root.join("store/config.toml").is_file());
    assert_eq!(
        LocalStore::open(link)
            .await
            .unwrap()
            .read_config()
            .unwrap()
            .get(ConfigKey::UserName),
        Some("foo")
    );
}
