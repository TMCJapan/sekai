//! `cache clean` driven through the CLI over a local store: the wipe is
//! wired to the store, idempotent, and leaves commits working (the cache
//! is derived state, so cleaning costs performance only).

use clap::Parser as _;
use sekai_app::SekaiInstance;
use sekai_cli::cli::Cli;

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
    let printer = sekai_cli::envelope::Printer::new(
        sekai_cli::style::Styler::new(cli.color),
        cli.command.output_json(),
    );
    sekai_cli::commands::run(&cli, &printer).await
}

/// Every commit in the store, oldest first is not guaranteed; the count
/// is all the callers assert.
async fn all_commits(instance: &SekaiInstance<impl sekai_app::SekaiStore>) -> usize {
    instance
        .log(
            &[],
            sekai_app::LogOptions {
                all: true,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .len()
}

/// A framed (uncompressed) level chunk whose only content is a
/// distinguishable `Status`, so ingest can hash it.
fn chunk_nbt(marker: &str) -> Vec<u8> {
    fn name(out: &mut Vec<u8>, key: &str) {
        out.extend_from_slice(&u16::try_from(key.len()).unwrap().to_be_bytes());
        out.extend_from_slice(key.as_bytes());
    }
    fn int(out: &mut Vec<u8>, key: &str, value: i32) {
        out.push(3);
        name(out, key);
        out.extend_from_slice(&value.to_be_bytes());
    }
    fn long(out: &mut Vec<u8>, key: &str, value: i64) {
        out.push(4);
        name(out, key);
        out.extend_from_slice(&value.to_be_bytes());
    }
    fn string(out: &mut Vec<u8>, key: &str, value: &str) {
        out.push(8);
        name(out, key);
        out.extend_from_slice(&u16::try_from(value.len()).unwrap().to_be_bytes());
        out.extend_from_slice(value.as_bytes());
    }
    fn empty_list(out: &mut Vec<u8>, key: &str) {
        out.push(9);
        name(out, key);
        out.push(0);
        out.extend_from_slice(&0i32.to_be_bytes());
    }
    fn empty_compound(out: &mut Vec<u8>, key: &str) {
        out.push(10);
        name(out, key);
        out.push(0);
    }

    let mut nbt = vec![10, 0, 0];
    int(&mut nbt, "DataVersion", 1);
    int(&mut nbt, "xPos", 0);
    int(&mut nbt, "yPos", 0);
    int(&mut nbt, "zPos", 0);
    long(&mut nbt, "InhabitedTime", 0);
    long(&mut nbt, "LastUpdate", 0);
    empty_list(&mut nbt, "sections");
    empty_compound(&mut nbt, "Heightmaps");
    string(&mut nbt, "Status", marker);
    empty_list(&mut nbt, "block_entities");
    empty_list(&mut nbt, "block_ticks");
    empty_list(&mut nbt, "fluid_ticks");
    nbt.push(0);

    let mut payload = vec![3]; // Uncompressed header byte
    payload.extend(nbt);
    payload
}

/// Write one region image holding a single chunk through the real builder.
fn write_region(path: impl AsRef<std::path::Path>) {
    let path = path.as_ref();
    let mut builder = sekai_anvil::RegionBuilder::new(0, 0, 0).unwrap();
    builder.stage_chunk(0, 0, &chunk_nbt("marker-1")).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, builder.image().unwrap()).unwrap();
}

#[tokio::test]
async fn cache_clean_wipes_the_store_cache_and_commits_keep_working() {
    let (root, _guard) = tempdir("cache-clean");
    let world = root.join("world");
    write_region(world.join("region/r.0.0.mca"));
    let store = root.join("store");
    let store = store.to_str().expect("temporary store path is UTF-8");
    let world = world.to_str().expect("temporary world path is UTF-8");

    run(&["sekai", "--world", world, "--store", store, "init"])
        .await
        .expect("init creates the store");
    // An empty cache cleans to zero, in both output modes.
    run(&[
        "sekai", "--world", world, "--store", store, "cache", "clean",
    ])
    .await
    .expect("clean of an empty cache");
    run(&[
        "sekai", "--world", world, "--store", store, "cache", "clean", "--json",
    ])
    .await
    .expect("clean of an empty cache, JSON mode");

    run(&["sekai", "--world", world, "--store", store, "commit"])
        .await
        .expect("commit records the region");
    // The commit populated one entry: an app-level wipe proves it.
    let mut instance = SekaiInstance::open_location(&outside(store)).await.unwrap();
    assert_eq!(instance.clean_cache().await.unwrap(), 1);
    drop(instance);

    // Repopulate, then wipe through the CLI: the follow-up app-level wipe
    // finds nothing left, so the CLI run removed the entry itself.
    run(&["sekai", "--world", world, "--store", store, "commit"])
        .await
        .expect("commit rebuilds the wiped entry");
    run(&[
        "sekai", "--world", world, "--store", store, "cache", "clean",
    ])
    .await
    .expect("cache clean");
    let mut instance = SekaiInstance::open_location(&outside(store)).await.unwrap();
    assert_eq!(instance.clean_cache().await.unwrap(), 0);
    drop(instance);

    // Cleaning never changes behavior: the same world still commits.
    run(&["sekai", "--world", world, "--store", store, "commit"])
        .await
        .expect("commit after clean");
    run(&["sekai", "--world", world, "--store", store, "status"])
        .await
        .expect("status after clean");
}

#[tokio::test]
async fn cache_clean_on_a_missing_store_fails() {
    let (root, _guard) = tempdir("cache-clean-missing");
    let store = root.join("store");
    let store = store.to_str().expect("temporary store path is UTF-8");
    let start = root.to_str().expect("temporary root path is UTF-8");
    let err = run(&[
        "sekai", "--world", start, "--store", store, "cache", "clean",
    ])
    .await
    .expect_err("a missing store must fail");
    assert!(
        err.chain()
            .any(|cause| cause.to_string().contains("no store at")),
        "failed for the wrong reason: {err:?}"
    );
    assert!(
        !root.join("store").exists(),
        "clean must not create a store"
    );
}

#[tokio::test]
async fn cache_update_populates_the_cache_without_a_commit() {
    let (root, _guard) = tempdir("cache-update");
    let world = root.join("world");
    write_region(world.join("region/r.0.0.mca"));
    let store = root.join("store");
    let store = store.to_str().expect("temporary store path is UTF-8");
    let world = world.to_str().expect("temporary world path is UTF-8");

    run(&["sekai", "--world", world, "--store", store, "init"])
        .await
        .expect("init creates the store");
    // Two runs (human and JSON) populate one entry idempotently.
    run(&[
        "sekai", "--world", world, "--store", store, "cache", "update",
    ])
    .await
    .expect("cache update");
    run(&[
        "sekai", "--world", world, "--store", store, "cache", "update", "--json",
    ])
    .await
    .expect("cache update, JSON mode");

    // One cache entry, but no commit was recorded.
    let mut instance = SekaiInstance::open_location(&outside(store)).await.unwrap();
    assert_eq!(all_commits(&instance).await, 0);
    assert_eq!(instance.clean_cache().await.unwrap(), 1);
    drop(instance);

    // The world still commits cleanly afterwards.
    run(&["sekai", "--world", world, "--store", store, "commit"])
        .await
        .expect("commit after cache update");
}

#[tokio::test]
async fn commit_no_cache_records_history_but_leaves_the_cache_empty() {
    let (root, _guard) = tempdir("commit-no-cache");
    let world = root.join("world");
    write_region(world.join("region/r.0.0.mca"));
    let store = root.join("store");
    let store = store.to_str().expect("temporary store path is UTF-8");
    let world = world.to_str().expect("temporary world path is UTF-8");

    run(&["sekai", "--world", world, "--store", store, "init"])
        .await
        .expect("init creates the store");

    // `--no-cache` records the commit but skips the update pass, so the
    // cache stays empty even though the region was ingested.
    run(&[
        "sekai",
        "--world",
        world,
        "--store",
        store,
        "commit",
        "--no-cache",
    ])
    .await
    .expect("commit --no-cache records the region");
    let mut instance = SekaiInstance::open_location(&outside(store)).await.unwrap();
    assert_eq!(all_commits(&instance).await, 1);
    assert_eq!(instance.clean_cache().await.unwrap(), 0);
    drop(instance);

    // A normal commit afterwards still warms the cache.
    run(&["sekai", "--world", world, "--store", store, "commit"])
        .await
        .expect("commit warms the cache");
    let mut instance = SekaiInstance::open_location(&outside(store)).await.unwrap();
    assert_eq!(instance.clean_cache().await.unwrap(), 1);
    drop(instance);
}

#[tokio::test]
async fn cache_update_on_a_missing_store_fails() {
    let (root, _guard) = tempdir("cache-update-missing");
    let store = root.join("store");
    let store = store.to_str().expect("temporary store path is UTF-8");
    let err = run(&["sekai", "--store", store, "cache", "update", "--world", "."])
        .await
        .expect_err("a missing store must fail");
    assert!(
        err.chain()
            .any(|cause| cause.to_string().contains("no store at")),
        "failed for the wrong reason: {err:?}"
    );
    assert!(
        !root.join("store").exists(),
        "update must not create a store"
    );
}

/// The store at `store`, opened outside every worktree.
fn outside(store: &str) -> sekai_app::Location {
    sekai_app::Location {
        store: store.into(),
        world: None,
    }
}
