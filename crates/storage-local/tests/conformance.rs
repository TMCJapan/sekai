//! Local backend conformance: shared flows plus filesystem-specific checks.

mod common;

use sekai_storage::{
    BlobStore as _, CacheStore as _, MetaStore as _, SekaiStore as _, SekaiStoreOpener,
};
use sekai_storage_local::LocalStore;
use sekai_util::{ChunkCoord, ChunkHash, CommitEntry, Dimension, RegionHash, RegionKind};
use std::path::Path;

/// A temp dir whose name starts with the calling test's name (tempfile
/// appends a random suffix), removed when its guard is dropped at the end
/// of the test, panics included.
fn tempdir(name: &str) -> (std::path::PathBuf, tempfile::TempDir) {
    let guard = tempfile::Builder::new()
        .prefix(&format!("sekai-{name}-"))
        .tempdir()
        .unwrap();
    (guard.path().to_path_buf(), guard)
}

async fn init(dir: impl AsRef<Path>) -> sekai_storage_local::LocalStore {
    LocalStore::init(dir).await.unwrap()
}

#[tokio::test]
async fn sqlite_commit_lifecycle() {
    let (dir, _guard) = tempdir("lifecycle");
    let mut store = init(&dir).await;
    common::commit_lifecycle(store.meta_mut()).await;
}

#[tokio::test]
async fn sqlite_scope_flow() {
    let (dir, _guard) = tempdir("scopes");
    let mut store = init(&dir).await;
    common::scope_flow(store.meta_mut()).await;
}

#[tokio::test]
async fn sqlite_fresh_stats() {
    let (dir, _guard) = tempdir("fresh");
    let mut store = init(&dir).await;
    common::fresh_stats(store.meta_mut()).await;
}

#[tokio::test]
async fn sqlite_tree_bulk_reads() {
    let (dir, _guard) = tempdir("bulk-reads");
    let mut store = init(&dir).await;
    common::tree_bulk_reads(store.meta_mut()).await;
}

/// A vanished coordinate stops resolving: absence is the tombstone, and the
/// snapshot walk a checkout plan is built from drops the coordinate instead
/// of asking for a blob.
#[tokio::test]
async fn sqlite_tombstones_shape_and_walk_drop() {
    let (dir, _guard) = tempdir("tombstones");
    let mut store = init(&dir).await;
    let meta = store.meta_mut();
    let present = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
    let gone = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Region, 1, 0);
    let first = meta
        .apply_commit_incremental(
            1_000,
            &[],
            &[
                CommitEntry::new(present, Some(ChunkHash::new([1; 32]))),
                CommitEntry::new(gone, Some(ChunkHash::new([2; 32]))),
            ],
        )
        .await
        .unwrap()
        .id;
    let second = meta
        .apply_commit_incremental(2_000, &[first], &[CommitEntry::new(gone, None)])
        .await
        .unwrap()
        .id;

    assert!(meta.lookup_chunk(second, &gone).await.unwrap().is_none());
    assert_eq!(
        meta.lookup_chunk(second, &present).await.unwrap(),
        Some(CommitEntry::new(present, Some(ChunkHash::new([1; 32]))))
    );

    let mut visited = Vec::new();
    meta.visit_commit_chunks(second, |entry| {
        visited.push(*entry);
        true
    })
    .await
    .unwrap();
    assert_eq!(
        visited,
        vec![CommitEntry::new(present, Some(ChunkHash::new([1; 32])))]
    );
}

#[tokio::test]
async fn sqlite_history_graph() {
    let (dir, _guard) = tempdir("graph");
    let mut store = init(&dir).await;
    common::history_graph(store.meta_mut()).await;
}

#[tokio::test]
async fn sqlite_cas_roundtrip() {
    let (dir, _guard) = tempdir("cas");
    let mut store = init(&dir).await;
    common::cas_roundtrip(store.blob_mut()).await;
}

#[tokio::test]
async fn cache_roundtrip() {
    let (dir, _guard) = tempdir("cache");
    let mut store = init(&dir).await;
    common::cache_roundtrip(store.cache_mut()).await;
}

#[tokio::test]
async fn sqlite_torn_commit_leaves_orphans() {
    let (dir, _guard) = tempdir("torn-commit");
    let mut store = init(&dir).await;
    common::torn_commit_leaves_orphans(&mut store).await;
}

#[tokio::test]
async fn visit_lists_stored_hashes_and_missing_fetch_clears() {
    let (dir, _guard) = tempdir("visit");
    let mut store = init(&dir).await;
    let blobs = store.blob_mut();
    let hash = ChunkHash::new(sekai_util::hash_bytes(b"real"));
    assert!(blobs.put(&hash, b"real").await.unwrap());
    // The keyspace has no file names to skip, so visiting every key is the
    // whole story; a missing hash is a read error, not an ignorable name.
    let mut seen = Vec::new();
    blobs
        .visit_blobs(|h| {
            seen.push(*h);
            true
        })
        .await
        .unwrap();
    assert_eq!(seen, vec![hash]);
    let mut out = vec![1u8, 2, 3];
    let err = blobs
        .fetch_into(&ChunkHash::new([9; 32]), &mut out)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        sekai_storage::StorageError::BlobMissing { .. }
    ));
    assert!(out.is_empty(), "a failed fetch clears the buffer");
}

/// The blob keyspace is a sled directory beside `meta.sqlite`, its default
/// tree keyed by the blob hashes alone.
#[tokio::test]
async fn blob_database_is_created() {
    let (dir, _guard) = tempdir("blob-created");
    let mut store = init(&dir).await;
    let hash = ChunkHash::new(sekai_util::hash_bytes(b"payload"));
    assert!(store.blob_mut().put(&hash, b"payload").await.unwrap());
    store.blob_mut().sync().await.unwrap();
    drop(store);

    let blob_path = dir.join("blobs");
    assert!(blob_path.is_dir());
    let db = sled::open(&blob_path).unwrap();
    assert_eq!(
        db.get(hash.as_bytes()).unwrap().unwrap().as_ref(),
        b"payload"
    );
    assert_eq!(db.tree_names().len(), 1);
    drop(db);
    let _store = LocalStore::open(&dir).await.unwrap();
}

/// The cache keyspace is a sled directory beside `meta.sqlite`, the image
/// in the default tree and its hash in a side tree so a hash lookup never
/// reads image bytes.
#[tokio::test]
async fn cache_database_is_created() {
    let (dir, _guard) = tempdir("cache-created");
    let mut store = init(&dir).await;
    let region = RegionHash::new([7; 32]);
    let image = b"mca image";
    let mca_hash = sekai_util::hash_bytes(image);
    assert!(
        store
            .cache_mut()
            .put(region, mca_hash, image)
            .await
            .unwrap()
    );
    store.cache_mut().sync().await.unwrap();
    drop(store);

    let cache_path = dir.join("cache");
    assert!(cache_path.is_dir());
    let db = sled::open(&cache_path).unwrap();
    assert_eq!(db.get(region.as_bytes()).unwrap().unwrap().as_ref(), image);
    let hashes = db.open_tree("mca_hashes").unwrap();
    assert_eq!(
        hashes.get(region.as_bytes()).unwrap().unwrap().as_ref(),
        &mca_hash[..]
    );
    assert_eq!(db.tree_names().len(), 2);
    drop(hashes);
    drop(db);
    let _store = LocalStore::open(&dir).await.unwrap();
}

/// A wiped cache stays openable: `clear` removes entries, never the
/// keyspace.
#[tokio::test]
async fn cache_clear_keeps_the_keyspace_openable() {
    let (dir, _guard) = tempdir("cache-clear");
    let mut store = init(&dir).await;
    let region = RegionHash::new([7; 32]);
    let image = b"mca image";
    assert!(
        store
            .cache_mut()
            .put(region, sekai_util::hash_bytes(image), image)
            .await
            .unwrap()
    );
    assert_eq!(store.cache_mut().clear().await.unwrap(), 1);
    drop(store);

    let store = LocalStore::open(&dir).await.unwrap();
    assert_eq!(store.cache().mca_hash(region).await.unwrap(), None);
}

/// A store from before the cache existed is refused, not silently
/// upgraded: pre-release policy recreates instead of migrating.
#[tokio::test]
async fn open_of_a_pre_cache_store_fails() {
    let (dir, _guard) = tempdir("pre-cache");
    let store = init(&dir).await;
    drop(store);
    std::fs::remove_dir_all(dir.join("cache")).unwrap();
    let err = LocalStore::open(&dir).await.unwrap_err();
    assert!(
        matches!(err, sekai_storage::StorageError::StoreMissing { .. }),
        "unexpected error: {err}"
    );
}

/// Init materializes `conf.toml` with the current config version, and a
/// re-init keeps the existing file instead of rewriting it.
#[tokio::test]
async fn init_writes_conf_and_reinit_keeps_it() {
    let (dir, _guard) = tempdir("conf-init");
    let store = init(&dir).await;
    drop(store);
    let conf = dir.join("conf.toml");
    assert_eq!(
        std::fs::read_to_string(&conf).unwrap(),
        format!("version = {}\n", sekai_storage_local::CONFIG_VERSION)
    );

    // A comment only a preserved file can carry: re-init must not rewrite it.
    std::fs::write(
        &conf,
        format!(
            "# kept\nversion = {}\n",
            sekai_storage_local::CONFIG_VERSION
        ),
    )
    .unwrap();
    let store = LocalStore::init(&dir).await.unwrap();
    drop(store);
    assert!(
        std::fs::read_to_string(&conf)
            .unwrap()
            .starts_with("# kept")
    );
}

/// A future config version is refused loudly instead of reinterpreted.
#[tokio::test]
async fn open_gates_the_config_version() {
    let (dir, _guard) = tempdir("conf-version");
    let store = init(&dir).await;
    drop(store);
    std::fs::write(dir.join("conf.toml"), "version = 99\n").unwrap();
    let err = LocalStore::open(&dir).await.unwrap_err();
    assert!(
        matches!(
            err,
            sekai_storage::StorageError::UnsupportedSchema { found: 99, supported }
                if supported == i64::from(sekai_storage_local::CONFIG_VERSION)
        ),
        "unexpected error: {err}"
    );
}

/// A malformed config fails loudly instead of being reinterpreted.
#[tokio::test]
async fn open_of_a_malformed_conf_fails() {
    let (dir, _guard) = tempdir("conf-malformed");
    let store = init(&dir).await;
    drop(store);
    std::fs::write(dir.join("conf.toml"), "version = \"one\"\n").unwrap();
    let err = LocalStore::open(&dir).await.unwrap_err();
    assert!(
        matches!(
            err,
            sekai_storage::StorageError::Backend {
                backend: "toml",
                ..
            }
        ),
        "unexpected error: {err}"
    );
}

/// `conf.toml` is part of the layout markers: a store without one is not
/// openable, and nothing is opened or created to find that out.
#[tokio::test]
async fn open_of_a_store_without_conf_fails() {
    let (dir, _guard) = tempdir("conf-missing");
    let store = init(&dir).await;
    drop(store);
    std::fs::remove_file(dir.join("conf.toml")).unwrap();
    assert!(!sekai_storage_local::local_store_exists(&dir));
    let err = LocalStore::open(&dir).await.unwrap_err();
    assert!(
        matches!(err, sekai_storage::StorageError::StoreMissing { .. }),
        "unexpected error: {err}"
    );
}

/// sled admits a single opener per keyspace: a second open fails while the
/// first handle lives, and succeeds once it is gone.
#[tokio::test]
async fn a_live_blob_keyspace_refuses_a_second_opener() {
    let (dir, _guard) = tempdir("exclusive");
    let store = init(&dir).await;
    let err = LocalStore::open(&dir).await.unwrap_err();
    assert!(
        matches!(
            err,
            sekai_storage::StorageError::Backend {
                backend: "sled",
                ..
            }
        ),
        "unexpected error: {err}"
    );
    drop(store);
    let _store = LocalStore::open(&dir).await.unwrap();
}

/// Only `LocalStore::init` materializes a store; `LocalStore::open` must fail loudly on
/// a root that was never initialized and leave nothing behind (issue #80).
#[tokio::test]
async fn open_of_a_missing_store_fails_without_creating_it() {
    let (base, _guard) = tempdir("missing");
    let dir = base.join("store");
    assert!(!sekai_storage_local::local_store_exists(&dir));
    let err = LocalStore::open(&dir).await.unwrap_err();
    assert!(
        matches!(&err, sekai_storage::StorageError::StoreMissing { path } if Path::new(path) == dir),
        "unexpected error: {err}"
    );
    assert!(!dir.exists(), "a failed open must not create anything");

    let store = LocalStore::init(&dir).await.unwrap();
    assert!(dir.join("conf.toml").is_file());
    assert!(dir.join("meta.sqlite").is_file());
    assert!(dir.join("blobs/conf").is_file());
    assert!(dir.join("cache/conf").is_file());
    assert!(sekai_storage_local::local_store_exists(&dir));
    // The keyspace admits one opener: release the init handle first.
    drop(store);
    let _store = LocalStore::open(&dir).await.unwrap();
}

/// The opener trait accepts any `AsRef<Path>` holder, not just strings, and
/// keeps the open/init exclusivity of the free functions (issue #80).
#[tokio::test]
async fn opener_trait_takes_path_like_locations() {
    let (base, _guard) = tempdir("opener-trait");
    let dir = base.join("store");

    let err = LocalStore::open(&dir).await.unwrap_err();
    assert!(
        matches!(&err, sekai_storage::StorageError::StoreMissing { path } if Path::new(path) == dir),
        "unexpected error: {err}"
    );
    assert!(!dir.exists(), "a failed open must not create anything");

    let store = LocalStore::init(dir.clone()).await.unwrap();
    assert!(sekai_storage_local::local_store_exists(&dir));
    // The keyspace admits one opener: release the init handle first.
    drop(store);

    let url = dir.to_string_lossy().into_owned();
    let store = LocalStore::open(url.as_str()).await.unwrap();
    drop(store);
    let _store = LocalStore::open(dir.as_path()).await.unwrap();
}
