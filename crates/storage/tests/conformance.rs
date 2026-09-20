//! SQLite backend conformance: shared flows plus filesystem-specific checks.
//!
//! Requires the `backend-sqlite` feature.

#![cfg(feature = "backend-sqlite")]

mod common;

use sekai_core::{BlobHash, BlobStore as _};
use sekai_storage::open_sqlite;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sekai-{}-{}-{}",
        name,
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

async fn open(dir: &Path) -> sekai_storage::SqliteStore {
    open_sqlite(dir).await.unwrap()
}

fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn sqlite_snapshot_lifecycle() {
    let dir = tempdir("snap");
    let mut store = open(&dir).await;
    common::snapshot_lifecycle(store.meta_mut()).await;
    cleanup(&dir);
}

#[tokio::test]
async fn sqlite_backup_carry() {
    let dir = tempdir("carry");
    let mut store = open(&dir).await;
    common::backup_carry(store.meta_mut()).await;
    cleanup(&dir);
}

#[tokio::test]
async fn sqlite_tombstones() {
    let dir = tempdir("tomb");
    let mut store = open(&dir).await;
    common::tombstones(store.meta_mut()).await;
    cleanup(&dir);
}

#[tokio::test]
async fn sqlite_tags() {
    let dir = tempdir("tags");
    let mut store = open(&dir).await;
    common::tags(store.meta_mut()).await;
    cleanup(&dir);
}

#[tokio::test]
async fn sqlite_fresh_stats() {
    let dir = tempdir("fresh");
    let mut store = open(&dir).await;
    common::fresh_stats(store.meta_mut()).await;
    cleanup(&dir);
}

#[tokio::test]
async fn sqlite_prune_flow() {
    let dir = tempdir("prune");
    let mut store = open(&dir).await;
    common::prune_flow(store.meta_mut()).await;
    cleanup(&dir);
}

#[tokio::test]
async fn sqlite_cas_roundtrip() {
    let dir = tempdir("cas");
    let mut store = open(&dir).await;
    common::cas_roundtrip(store.cas_mut()).await;
    cleanup(&dir);
}

#[tokio::test]
async fn sqlite_torn_orphan_gc() {
    let dir = tempdir("gc");
    let mut store = open(&dir).await;
    common::torn_orphan_gc(&mut store).await;
    cleanup(&dir);
}

#[tokio::test]
async fn cas_skips_foreign_and_temp_names() {
    let dir = tempdir("names");
    let mut store = open(&dir).await;
    let cas = store.cas_mut();
    let hash = sekai_core::hash_blob(b"real");
    assert!(cas.put(&hash, b"real").await.unwrap());
    let hex = hash.hex_string();
    let shard = cas.root().join("blobs").join(&hex[..2]);
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
    assert!(out.is_empty());
    cleanup(&dir);
}
