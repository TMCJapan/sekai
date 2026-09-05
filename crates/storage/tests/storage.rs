//! Persistence contract: CAS files and SQLite history across reopens.
//!
//! Rationale: these tests pin what `engine` relies on - blobs land at the
//! documented fanout path and survive process restart, snapshots batch
//! atomically, tombstones round-trip as `NULL`, and a foreign schema
//! version fails loudly instead of misreading.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use sekai_core::{
    BlobHash, BlobStore as _, ChunkCoord, DiffHash, Dimension, MetaStore as _, RegionKind,
    SnapshotId,
};
use sekai_storage::{FileCas, SnapshotEntry, SqliteMeta, StorageError};

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "sekai-storage-test-{}-{}",
            std::process::id(),
            DIR_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

const fn hash(byte: u8) -> BlobHash {
    BlobHash([byte; 32])
}

const fn coord(x: i32, z: i32) -> ChunkCoord {
    ChunkCoord::new(Dimension::OVERWORLD, RegionKind::REGION, x, z)
}

#[test]
fn cas_round_trip_with_dedup_and_layout() {
    let dir = ScratchDir::new();
    let mut cas = FileCas::open(&dir.path).unwrap();
    let h = hash(0xAB);
    let payload = b"exact chunk bytes";

    assert!(cas.put(&h, payload).unwrap());
    // Second write of identical bytes is a dedup no-op.
    assert!(!cas.put(&h, payload).unwrap());
    assert!(cas.contains(&h).unwrap());
    assert!(!cas.contains(&hash(0xCD)).unwrap());

    // Documented fanout: blobs/ab/<remaining 62 hex chars>.
    let hex = h.hex_string();
    let expect = dir.path.join("blobs").join(&hex[..2]).join(&hex[2..]);
    assert_eq!(fs::read(&expect).unwrap(), payload);

    // Fetch clears the buffer first and returns exact bytes.
    let mut out = vec![1, 2, 3];
    cas.fetch_into(&h, &mut out).unwrap();
    assert_eq!(out, payload);

    // Missing blobs are corruption errors naming the absent hash.
    let missing = hash(0xCD);
    let err = cas.fetch_into(&missing, &mut Vec::new()).unwrap_err();
    assert!(matches!(err, StorageError::BlobMissing { .. }));
    assert!(format!("{err}").contains(&missing.hex_string()));
}

#[test]
fn cas_survives_reopen() {
    let dir = ScratchDir::new();
    let h = hash(7);
    FileCas::open(&dir.path).unwrap().put(&h, b"data").unwrap();
    // Fresh handle over the same root sees the blob.
    let cas = FileCas::open(&dir.path).unwrap();
    let mut out = Vec::new();
    cas.fetch_into(&h, &mut out).unwrap();
    assert_eq!(out, b"data");
}

#[test]
fn meta_snapshot_batch_lookup_and_tombstone() {
    let dir = ScratchDir::new();
    let db = dir.path.join("meta.sqlite");
    let mut meta = SqliteMeta::open(&db).unwrap();

    let s1 = meta
        .apply_snapshot(
            1_000,
            &[
                SnapshotEntry::new(coord(0, 0), Some(hash(1)), Some(DiffHash([9; 32]))),
                SnapshotEntry::new(coord(1, 0), None, None),
            ],
        )
        .unwrap();
    assert_eq!(s1, SnapshotId(1));

    // Present chunk: blob + diff round-trip exactly.
    let got = meta.lookup_chunk(s1, &coord(0, 0)).unwrap().unwrap();
    assert_eq!(got.blob, Some(hash(1)));
    assert_eq!(got.diff, Some(DiffHash([9; 32])));
    // Tombstone: explicit absence, still a row.
    let tomb = meta.lookup_chunk(s1, &coord(1, 0)).unwrap().unwrap();
    assert!(tomb.is_tombstone());
    // Never recorded: no row at all.
    assert!(meta.lookup_chunk(s1, &coord(9, 9)).unwrap().is_none());

    // Second snapshot reuses blobs (dedup is global, history is per-snap).
    let s2 = meta
        .apply_snapshot(
            2_000,
            &[SnapshotEntry::new(coord(0, 0), Some(hash(1)), None)],
        )
        .unwrap();
    assert_eq!(s2, SnapshotId(2));
    let got = meta.lookup_chunk(s2, &coord(0, 0)).unwrap().unwrap();
    assert_eq!(got.blob, Some(hash(1)));
    assert_eq!(got.diff, None);

    // Visits stream in order and honor early stop.
    let mut ids = Vec::new();
    meta.visit_snapshots(|s| {
        ids.push(s.id);
        true
    })
    .unwrap();
    assert_eq!(ids, vec![SnapshotId(1), SnapshotId(2)]);
    let mut count = 0;
    meta.visit_snapshot_chunks(s1, |_| {
        count += 1;
        false
    })
    .unwrap();
    assert_eq!(count, 1);

    // Same coordinate twice in one snapshot is a caller bug: hard error.
    let s3 = meta.create_snapshot(3_000).unwrap();
    meta.record_chunk(s3, &coord(0, 0), Some(&hash(2)), None)
        .unwrap();
    assert!(
        meta.record_chunk(s3, &coord(0, 0), Some(&hash(2)), None)
            .is_err()
    );
}

#[test]
fn meta_survives_reopen_and_rejects_foreign_schema() {
    let dir = ScratchDir::new();
    let db = dir.path.join("meta.sqlite");
    let mut meta = SqliteMeta::open(&db).unwrap();
    let s1 = meta
        .apply_snapshot(42, &[SnapshotEntry::new(coord(3, 3), Some(hash(5)), None)])
        .unwrap();
    drop(meta);

    // Data persists across handles (WAL sidecars included).
    let meta = SqliteMeta::open(&db).unwrap();
    let got = meta.lookup_chunk(s1, &coord(3, 3)).unwrap().unwrap();
    assert_eq!(got.blob, Some(hash(5)));

    // A newer schema version fails loudly instead of misreading.
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch("PRAGMA user_version=99")
        .unwrap();
    let err = SqliteMeta::open(&db).unwrap_err();
    assert!(matches!(
        err,
        StorageError::UnsupportedSchema { found: 99, .. }
    ));
}
