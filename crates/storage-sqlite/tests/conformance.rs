//! SQLite backend conformance: shared flows plus filesystem-specific checks.

mod common;

use sekai_core::{
    BlobHash, BlobStore as _, ChunkCoord, ChunkHistoryEntry, CommitEntry, Dimension,
    MetaStore as _, RegionKind, usecase::checkout::plan_checkout,
};
use sekai_storage::SekaiStore;
use sekai_storage_sqlite::{init_sqlite, open_sqlite};
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

async fn init(dir: impl AsRef<Path>) -> sekai_storage_sqlite::SqliteStore {
    init_sqlite(dir).await.unwrap()
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
                CommitEntry::new(present, Some(BlobHash([1; 32])), None),
                CommitEntry::new(gone, Some(BlobHash([2; 32])), None),
            ],
        )
        .await
        .unwrap()
        .id;
    let second = meta
        .apply_commit_incremental(2_000, &[first], &[CommitEntry::new(gone, None, None)])
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
    assert_eq!(
        fresh,
        vec![ChunkHistoryEntry::new(gone, second, None, None)]
    );

    let plan = plan_checkout(&*meta, second).await.unwrap();
    let region = sekai_core::RegionKey::of(present);
    assert_eq!(plan.groups[&region], vec![(present, BlobHash([1; 32]))]);
    assert_eq!(plan.tombstones[&region], vec![gone]);
}

#[tokio::test]
async fn sqlite_prune_flow() {
    let (dir, _guard) = tempdir("prune");
    let mut store = init(&dir).await;
    common::prune_flow(store.meta_mut()).await;
}

#[tokio::test]
async fn sqlite_prune_chain_flow() {
    let (dir, _guard) = tempdir("prune-chain");
    let mut store = init(&dir).await;
    common::prune_chain_flow(store.meta_mut()).await;
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
async fn sqlite_torn_orphan_gc() {
    let (dir, _guard) = tempdir("gc");
    let mut store = init(&dir).await;
    common::torn_orphan_gc(&mut store).await;
}

#[tokio::test]
async fn visit_lists_stored_hashes_and_missing_fetch_clears() {
    let (dir, _guard) = tempdir("visit");
    let mut store = init(&dir).await;
    let blobs = store.blob_mut();
    let hash = sekai_core::hash_blob(b"real");
    assert!(blobs.put(&hash, b"real").await.unwrap());
    // A table has no file names to skip, so visiting every row is the whole
    // story; a missing hash is a read error, not an ignorable name.
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
        .fetch_into(&BlobHash([9; 32]), &mut out)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        sekai_storage::StorageError::BlobMissing { .. }
    ));
    assert!(out.is_empty(), "a failed fetch clears the buffer");
}

/// The blob database is its own versioned file beside `meta.sqlite`.
#[tokio::test]
async fn blob_database_is_created_and_version_gated() {
    let (dir, _guard) = tempdir("blob-schema");
    let mut store = init(&dir).await;
    let hash = sekai_core::hash_blob(b"payload");
    assert!(store.blob_mut().put(&hash, b"payload").await.unwrap());
    store.blob_mut().sync().await.unwrap();
    drop(store);

    let blob_path = dir.join("blob.sqlite");
    assert!(blob_path.is_file());
    let version: i64 = {
        let pool = sqlx::SqlitePool::connect_with(
            sqlx::sqlite::SqliteConnectOptions::new().filename(&blob_path),
        )
        .await
        .unwrap();
        let version = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&pool)
            .await
            .unwrap();
        pool.close().await;
        version
    };
    assert_eq!(version, 1);

    // A future version is refused loudly instead of reinterpreted.
    let foreign = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&blob_path)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal),
    )
    .await
    .unwrap();
    sqlx::query("PRAGMA user_version = 99")
        .execute(&foreign)
        .await
        .unwrap();
    foreign.close().await;
    let err = open_sqlite(&dir).await.unwrap_err();
    assert!(matches!(
        err,
        sekai_storage::StorageError::UnsupportedSchema {
            found: 99,
            supported: 1
        }
    ));
}

/// The blocking side works from `spawn_blocking` workers, where ingest runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocking_blob_path_round_trips() {
    use sekai_storage::BlockingBlobStore as _;

    let (dir, _guard) = tempdir("blocking");
    let store = init(&dir).await;
    let mut cas = store.blob().clone();
    let hash = sekai_core::hash_blob(b"blocking");
    tokio::task::spawn_blocking(move || {
        assert!(!cas.contains_blob(&hash).unwrap());
        assert!(cas.put_blob(&hash, b"blocking").unwrap());
        assert!(!cas.put_blob(&hash, b"blocking").unwrap());
        cas.sync_dirs().unwrap();
        let mut out = Vec::new();
        cas.fetch_blob(&hash, &mut out).unwrap();
        assert_eq!(out, b"blocking");
        // The blocking guard is the same store lock the async side takes.
        let guard = cas.lock(sekai_storage::StoreLockMode::Writer).unwrap();
        assert!(
            cas.lock(sekai_storage::StoreLockMode::Writer).is_err(),
            "a second writer is excluded"
        );
        drop(guard);
        cas.lock(sekai_storage::StoreLockMode::Writer).unwrap();
    })
    .await
    .unwrap();
    assert!(store.blob().contains(&hash).await.unwrap());
}

/// Only `init_sqlite` materializes a store; `open_sqlite` must fail loudly on
/// a root that was never initialized and leave nothing behind (issue #80).
#[tokio::test]
async fn open_of_a_missing_store_fails_without_creating_it() {
    let (base, _guard) = tempdir("missing");
    let dir = base.join("store");
    let err = open_sqlite(&dir).await.unwrap_err();
    assert!(
        matches!(&err, sekai_storage::StorageError::StoreMissing { path } if path == &dir),
        "unexpected error: {err}"
    );
    assert!(!dir.exists(), "a failed open must not create anything");

    let _store = init_sqlite(&dir).await.unwrap();
    assert!(dir.join("meta.sqlite").is_file());
    assert!(dir.join("blob.sqlite").is_file());
    let _store = open_sqlite(&dir).await.unwrap();
}
