//! SQLite backend conformance: shared flows plus filesystem-specific checks.

mod common;

use sekai_core::{
    BlobHash, BlobStore as _, ChunkCoord, ChunkHistoryEntry, CommitEntry, Dimension,
    MetaStore as _, RegionKind, usecase::checkout::plan_checkout,
};
use sekai_storage_sqlite::{init_sqlite, open_sqlite};
use std::path::Path;
fn tempdir(_name: &str) -> std::path::PathBuf {
    tempfile::tempdir().unwrap().keep()
}

async fn init(dir: impl AsRef<Path>) -> sekai_storage_sqlite::SqliteStore {
    init_sqlite(dir).await.unwrap()
}

fn cleanup(dir: impl AsRef<Path>) {
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn sqlite_commit_lifecycle() {
    let dir = tempdir("lifecycle");
    let mut store = init(&dir).await;
    common::commit_lifecycle(store.meta_mut()).await;
    cleanup(&dir);
}

#[tokio::test]
async fn sqlite_scope_flow() {
    let dir = tempdir("scopes");
    let mut store = init(&dir).await;
    common::scope_flow(store.meta_mut()).await;
    cleanup(&dir);
}

#[tokio::test]
async fn sqlite_fresh_stats() {
    let dir = tempdir("fresh");
    let mut store = init(&dir).await;
    common::fresh_stats(store.meta_mut()).await;
    cleanup(&dir);
}

/// A vanished coordinate keeps resolving as a tombstone: the effective row
/// is present at the new commit, the only fresh row is the tombstone, and a
/// checkout plan drops the coordinate instead of asking for a blob.
#[tokio::test]
async fn sqlite_tombstones_shape_and_checkout_drop() {
    let dir = tempdir("tombstones");
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
    cleanup(&dir);
}

#[tokio::test]
async fn sqlite_prune_flow() {
    let dir = tempdir("prune");
    let mut store = init(&dir).await;
    common::prune_flow(store.meta_mut()).await;
    cleanup(&dir);
}

#[tokio::test]
async fn sqlite_prune_chain_flow() {
    let dir = tempdir("prune-chain");
    let mut store = init(&dir).await;
    common::prune_chain_flow(store.meta_mut()).await;
    cleanup(&dir);
}

#[tokio::test]
async fn sqlite_history_graph() {
    let dir = tempdir("graph");
    let mut store = init(&dir).await;
    common::history_graph(store.meta_mut()).await;
    cleanup(&dir);
}

#[tokio::test]
async fn sqlite_cas_roundtrip() {
    let dir = tempdir("cas");
    let mut store = init(&dir).await;
    common::cas_roundtrip(store.cas_mut()).await;
    cleanup(&dir);
}

#[tokio::test]
async fn sqlite_torn_orphan_gc() {
    let dir = tempdir("gc");
    let mut store = init(&dir).await;
    common::torn_orphan_gc(&mut store).await;
    cleanup(&dir);
}

#[tokio::test]
async fn cas_skips_foreign_and_temp_names() {
    let dir = tempdir("names");
    let mut store = init(&dir).await;
    let cas = store.cas_mut();
    let hash = sekai_core::hash_blob(b"real");
    assert!(cas.put(&hash, b"real").await.unwrap());
    let hex = hash.hex_string();
    let shard = cas.root().as_ref().join("blobs").join(&hex[..2]);
    std::fs::write(shard.join("foreign.bin"), b"x").unwrap();
    std::fs::write(shard.join(format!("{}-tmp", &hex[2..])), b"y").unwrap();
    let mut seen = Vec::new();
    cas.visit_blobs(|h| {
        seen.push(*h);
        true
    })
    .await
    .unwrap();
    assert_eq!(seen, vec![hash]);
    let mut out = Vec::new();
    assert!(cas.fetch_into(&BlobHash([9; 32]), &mut out).await.is_err());
    assert_eq!(out, [] as [u8; 0]);
    cleanup(&dir);
}

/// Only `init_sqlite` materializes a store; `open_sqlite` must fail loudly on
/// a root that was never initialized and leave nothing behind (issue #80).
#[tokio::test]
async fn open_of_a_missing_store_fails_without_creating_it() {
    let base = tempdir("missing");
    let dir = base.join("store");
    let err = open_sqlite(&dir).await.unwrap_err();
    assert!(
        matches!(&err, sekai_storage::StorageError::StoreMissing { path } if path == &dir),
        "unexpected error: {err}"
    );
    assert!(!dir.exists(), "a failed open must not create anything");

    let _store = init_sqlite(&dir).await.unwrap();
    assert!(dir.join("meta.sqlite").is_file());
    assert!(dir.join("blobs").is_dir());
    let _store = open_sqlite(&dir).await.unwrap();
    cleanup(&base);
}
