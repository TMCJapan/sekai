//! Local backend conformance: shared flows plus filesystem-specific checks.

mod common;

use sekai_core::{
    BlobStore as _, ChunkCoord, ChunkHash, ChunkHistoryEntry, CommitEntry, Dimension,
    MetaStore as _, RegionKind, usecase::checkout::plan_checkout,
};
use sekai_storage_local::{init_local, open_local};
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
    init_local(dir).await.unwrap()
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

/// A vanished coordinate keeps resolving as a tombstone: the effective row
/// is present at the new commit, the only fresh row is the tombstone, and a
/// checkout plan drops the coordinate instead of asking for a blob.
#[tokio::test]
async fn sqlite_tombstones_shape_and_checkout_drop() {
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

    let row = meta.lookup_chunk(second, &gone).await.unwrap().unwrap();
    assert!(row.is_tombstone());
    let mut fresh = Vec::new();
    meta.visit_fresh_rows(second, |row| {
        fresh.push(*row);
        true
    })
    .await
    .unwrap();
    assert_eq!(fresh, vec![ChunkHistoryEntry::new(gone, second, None)]);

    let plan = plan_checkout(&*meta, second).await.unwrap();
    let region = sekai_core::RegionKey::of(present);
    assert_eq!(
        plan.groups[&region],
        vec![(present, ChunkHash::new([1; 32]))]
    );
    assert_eq!(plan.tombstones[&region], vec![gone]);
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
    let hash = ChunkHash::new(sekai_core::hash_bytes(b"real"));
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

/// The blob keyspace is its own versioned sled directory beside `meta.sqlite`.
#[tokio::test]
async fn blob_database_is_created_and_version_gated() {
    let (dir, _guard) = tempdir("blob-schema");
    let mut store = init(&dir).await;
    let hash = ChunkHash::new(sekai_core::hash_bytes(b"payload"));
    assert!(store.blob_mut().put(&hash, b"payload").await.unwrap());
    store.blob_mut().sync().await.unwrap();
    drop(store);

    let blob_path = dir.join("blobs");
    assert!(blob_path.is_dir());
    // The version marker lives in a reserved tree, apart from the blob
    // hashes; the tree and key names pin the on-disk format.
    let db = sled::open(&blob_path).unwrap();
    let versions = db.open_tree("__sekai_version").unwrap();
    let stored = versions.get("schema").unwrap().unwrap();
    let version = i64::from_le_bytes(stored.as_ref().try_into().unwrap());
    assert_eq!(version, 1);

    // A future version is refused loudly instead of reinterpreted.
    versions.insert("schema", &99i64.to_le_bytes()[..]).unwrap();
    db.flush().unwrap();
    // Every tree handle shares the session: all of them must go before the
    // keyspace can be opened again.
    drop(versions);
    drop(db);
    let err = open_local(&dir).await.unwrap_err();
    assert!(matches!(
        err,
        sekai_storage::StorageError::UnsupportedSchema {
            found: 99,
            supported: 1
        }
    ));
}

/// sled admits a single opener per keyspace: a second open fails while the
/// first handle lives, and succeeds once it is gone.
#[tokio::test]
async fn a_live_blob_keyspace_refuses_a_second_opener() {
    let (dir, _guard) = tempdir("exclusive");
    let store = init(&dir).await;
    let err = open_local(&dir).await.unwrap_err();
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
    let _store = open_local(&dir).await.unwrap();
}

/// Only `init_local` materializes a store; `open_local` must fail loudly on
/// a root that was never initialized and leave nothing behind (issue #80).
#[tokio::test]
async fn open_of_a_missing_store_fails_without_creating_it() {
    let (base, _guard) = tempdir("missing");
    let dir = base.join("store");
    assert!(!sekai_storage_local::local_store_exists(&dir));
    let err = open_local(&dir).await.unwrap_err();
    assert!(
        matches!(&err, sekai_storage::StorageError::StoreMissing { path } if path == &dir),
        "unexpected error: {err}"
    );
    assert!(!dir.exists(), "a failed open must not create anything");

    let store = init_local(&dir).await.unwrap();
    assert!(dir.join("meta.sqlite").is_file());
    assert!(dir.join("blobs/conf").is_file());
    assert!(sekai_storage_local::local_store_exists(&dir));
    // The keyspace admits one opener: release the init handle first.
    drop(store);
    let _store = open_local(&dir).await.unwrap();
}
