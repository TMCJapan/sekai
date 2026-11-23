//! `restore` driven through the CLI over a local store: the git-restore
//! contract (required pathspec, default HEAD source, scoped writes) holds
//! end to end, and the run leaves history alone.

use clap::Parser as _;
use sekai_app::SekaiInstance;
use sekai_cli::cli::Cli;
use sekai_storage_local::LocalStore;

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

/// Write one region image through the real builder.
fn write_region(path: impl AsRef<std::path::Path>, chunks: &[(i32, i32, &str)]) {
    let path = path.as_ref();
    let name = path.file_name().unwrap().to_str().unwrap();
    let (rx, rz) = sekai_anvil::parse_region_name(name).unwrap();
    let mut builder = sekai_anvil::RegionBuilder::new(rx, rz, 0).unwrap();
    for (x, z, marker) in chunks {
        builder.stage_chunk(*x, *z, &chunk_nbt(marker)).unwrap();
    }
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, builder.image().unwrap()).unwrap();
}

/// The stored payload of the chunk at `(x, z)`, or `None` when absent.
fn payload_at(path: impl AsRef<std::path::Path>, x: i32, z: i32) -> Option<Vec<u8>> {
    let path = path.as_ref();
    let name = path.file_name().unwrap().to_str().unwrap();
    let (rx, rz) = sekai_anvil::parse_region_name(name).unwrap();
    let bytes = std::fs::read(path).unwrap();
    let image = sekai_anvil::RegionImage::from_bytes(bytes, rx, rz).unwrap();
    let mut found = None;
    image
        .visit_chunks(|chunk| {
            if (chunk.x, chunk.z) == (x, z) {
                found = Some(chunk.payload.to_vec());
                return false;
            }
            true
        })
        .unwrap();
    found
}

#[tokio::test]
async fn restore_reverts_the_selection_from_head() {
    let (root, _guard) = tempdir("restore-head");
    let world = root.join("world");
    let region = world.join("region/r.0.0.mca");
    write_region(&region, &[(0, 0, "marker-1")]);
    let store = root.join("store");
    let store = store.to_str().expect("temporary store path is UTF-8");
    let world = world.to_str().expect("temporary world path is UTF-8");

    run(&["sekai", "--store", store, "init"])
        .await
        .expect("init creates the store");
    run(&["sekai", "--store", store, "commit", world])
        .await
        .expect("commit records the region");
    write_region(&region, &[(0, 0, "marker-9")]);

    run(&[
        "sekai",
        "--store",
        store,
        "restore",
        world,
        "--in",
        "overworld:0,0",
    ])
    .await
    .expect("restore reverts the selection");
    assert_eq!(payload_at(&region, 0, 0), Some(chunk_nbt("marker-1")));

    // History is untouched: still exactly one commit.
    let instance = SekaiInstance::<LocalStore>::open(store).await.unwrap();
    assert_eq!(instance.list_commits().await.unwrap().len(), 1);
}

#[tokio::test]
async fn restore_refuses_an_empty_pathspec() {
    let (root, _guard) = tempdir("restore-pathspec");
    let world = root.join("world");
    write_region(world.join("region/r.0.0.mca"), &[(0, 0, "marker-1")]);
    let store = root.join("store");
    let store = store.to_str().expect("temporary store path is UTF-8");
    let world = world.to_str().expect("temporary world path is UTF-8");

    run(&["sekai", "--store", store, "init"])
        .await
        .expect("init creates the store");
    run(&["sekai", "--store", store, "commit", world])
        .await
        .expect("commit records the region");

    // git restore demands a pathspec; a bare invocation (or `--kind`
    // alone, which spells no path) must fail before touching the world.
    for args in [
        vec!["sekai", "--store", store, "restore", world],
        vec![
            "sekai", "--store", store, "restore", world, "--kind", "region",
        ],
    ] {
        let err = run(&args).await.expect_err("an empty pathspec is refused");
        assert!(
            err.to_string()
                .contains("you must specify path(s) to restore"),
            "{err}"
        );
    }
}

#[tokio::test]
async fn restore_source_and_overlay_flags() {
    let (root, _guard) = tempdir("restore-source");
    let world = root.join("world");
    let known = world.join("region/r.0.0.mca");
    let added = world.join("region/r.1.0.mca");
    write_region(&known, &[(0, 0, "marker-1")]);
    let store = root.join("store");
    let store = store.to_str().expect("temporary store path is UTF-8");
    let world = world.to_str().expect("temporary world path is UTF-8");

    run(&["sekai", "--store", store, "init"])
        .await
        .expect("init creates the store");
    run(&["sekai", "--store", store, "commit", world])
        .await
        .expect("commit records the region");
    write_region(&known, &[(0, 0, "marker-2")]);
    run(&["sekai", "--store", store, "commit", world])
        .await
        .expect("second commit records the divergence");
    write_region(&added, &[(32, 0, "marker-3")]);

    // An explicit older source reverts the known region and, no-overlay,
    // deletes the post-source file inside the selection.
    run(&[
        "sekai",
        "--store",
        store,
        "restore",
        world,
        "-s",
        "HEAD~1",
        "--in",
        "overworld",
    ])
    .await
    .expect("restore from an explicit source");
    assert_eq!(payload_at(&known, 0, 0), Some(chunk_nbt("marker-1")));
    assert!(!added.exists());

    // Overlay keeps the source-unknown file; HEAD still names commit 2.
    write_region(&added, &[(32, 0, "marker-3")]);
    run(&[
        "sekai",
        "--store",
        store,
        "restore",
        world,
        "--source",
        "HEAD~1",
        "--overlay",
        "--in",
        "overworld",
    ])
    .await
    .expect("overlay restore keeps unknown files");
    assert!(added.exists());
    let instance = SekaiInstance::<LocalStore>::open(store).await.unwrap();
    assert_eq!(instance.list_commits().await.unwrap().len(), 2);
}
