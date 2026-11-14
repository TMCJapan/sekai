//! Blob-level diffs: both stored payloads digest through the same
//! normalized view ingest uses, and every failure keeps its class.
//!
//! Requires the `backend-local` feature.

#![cfg(feature = "backend-local")]

mod fixtures;

use fixtures::chunk_nbt;
use sekai_anvil::ChunkHashError;
use sekai_app::{AppError, ChunkHash, RegionKind, SekaiInstance};
use sekai_core::BlobStore as _;
use sekai_storage::StorageError;
use sekai_storage_local::LocalStore;

async fn instance(name: &str) -> (SekaiInstance<LocalStore>, tempfile::TempDir) {
    let guard = tempfile::Builder::new()
        .prefix(&format!("sekai-app-diff-{name}-"))
        .tempdir()
        .unwrap();
    let url = guard.path().join("store").to_string_lossy().into_owned();
    let instance = SekaiInstance::init(&url).await.unwrap();
    (instance, guard)
}

/// CAS key of a framed payload, through the same normalization ingest uses.
fn chunk_key_of(framed: &[u8]) -> ChunkHash {
    sekai_anvil::Chunk::new(RegionKind::Region, 0, 0, framed)
        .normalized_hash()
        .unwrap()
}

#[tokio::test]
async fn missing_blob_is_a_backend_error() {
    let (instance, _guard) = instance("missing-blob").await;
    let missing = ChunkHash::new(sekai_core::hash_bytes(b"never-stored"));
    let err = instance.diff_blobs(&missing, &missing).await.unwrap_err();
    assert!(matches!(
        err,
        AppError::Storage(StorageError::BlobMissing { .. })
    ));
}

#[tokio::test]
async fn undecodable_payload_is_a_codec_error() {
    let (mut instance, _guard) = instance("bad-codec").await;
    // Unknown compression type byte: anvil rejects before NBT sees it.
    let bad_codec = vec![9u8, 0, 0];
    let hash_bad = ChunkHash::new(sekai_core::hash_bytes(&bad_codec));
    instance
        .store_mut()
        .blob_mut()
        .put(&hash_bad, &bad_codec)
        .await
        .unwrap();
    let good = chunk_nbt("minecraft:full");
    let hash_good = chunk_key_of(&good);
    instance
        .store_mut()
        .blob_mut()
        .put(&hash_good, &good)
        .await
        .unwrap();

    let err = instance
        .diff_blobs(&hash_bad, &hash_good)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        AppError::ChunkHash(ChunkHashError::Decompress(_))
    ));
}

#[tokio::test]
async fn corrupt_nbt_is_a_decode_error() {
    let (mut instance, _guard) = instance("corrupt-nbt").await;
    // Type 3 framing with truncated NBT body.
    let corrupt = vec![3u8, 10, 0];
    let hash_corrupt = ChunkHash::new(sekai_core::hash_bytes(&corrupt));
    instance
        .store_mut()
        .blob_mut()
        .put(&hash_corrupt, &corrupt)
        .await
        .unwrap();
    let good = chunk_nbt("minecraft:full");
    let hash_good = chunk_key_of(&good);
    instance
        .store_mut()
        .blob_mut()
        .put(&hash_good, &good)
        .await
        .unwrap();

    let err = instance
        .diff_blobs(&hash_corrupt, &hash_good)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::ChunkHash(_)));
}

#[tokio::test]
async fn diff_blobs_reports_full_for_different_payloads() {
    let (mut instance, _guard) = instance("different").await;
    let old = chunk_nbt("minecraft:full");
    let new = chunk_nbt("minecraft:empty");
    let hash_old = chunk_key_of(&old);
    let hash_new = chunk_key_of(&new);
    instance
        .store_mut()
        .blob_mut()
        .put(&hash_old, &old)
        .await
        .unwrap();
    instance
        .store_mut()
        .blob_mut()
        .put(&hash_new, &new)
        .await
        .unwrap();

    let diff = instance.diff_blobs(&hash_old, &hash_new).await.unwrap();
    assert!(
        matches!((diff.old, diff.new), (Some(old), Some(new)) if old != new),
        "different payloads must report a full diff with distinct hashes: {diff:?}"
    );
}

#[tokio::test]
async fn diff_blobs_reports_nothing_for_identical_payloads() {
    let (mut instance, _guard) = instance("identical").await;
    let payload = chunk_nbt("minecraft:full");
    let hash = chunk_key_of(&payload);
    instance
        .store_mut()
        .blob_mut()
        .put(&hash, &payload)
        .await
        .unwrap();

    let diff = instance.diff_blobs(&hash, &hash).await.unwrap();
    assert!(diff.is_empty());
}
