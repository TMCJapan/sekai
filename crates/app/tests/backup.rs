//! End-to-end backup flows over real world folders and SQLite stores.

use sekai_app::{BackupOptions, SnapshotId};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sekai-app-{name}-{}-{}",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

/// Write a region image through the real builder, deriving the region
/// coordinates from the file name.
fn write_region(path: &Path, chunks: &[(i32, i32, Vec<u8>)]) {
    let name = path.file_name().unwrap().to_str().unwrap();
    let (rx, rz) = sekai_anvil::parse_region_name(name).unwrap();
    let mut builder = sekai_anvil::RegionBuilder::new(rx, rz, 0).unwrap();
    for (x, z, payload) in chunks {
        builder.stage_chunk(*x, *z, payload).unwrap();
    }
    let image = builder.image().unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, image).unwrap();
}

/// Chunk coordinates present in the region file at `path`.
fn read_coords(path: &Path) -> Vec<(i32, i32)> {
    let name = path.file_name().unwrap().to_str().unwrap();
    let (rx, rz) = sekai_anvil::parse_region_name(name).unwrap();
    let bytes = std::fs::read(path).unwrap();
    let image = sekai_anvil::RegionImage::from_bytes(bytes, rx, rz).unwrap();
    let mut coords = Vec::new();
    image
        .visit_chunks(|chunk| {
            coords.push((chunk.x, chunk.z));
            true
        })
        .unwrap();
    coords
}

fn options() -> BackupOptions {
    BackupOptions {
        concurrency: 2,
        ..BackupOptions::default()
    }
}

#[tokio::test]
async fn backup_list_rollback_round_trip() {
    let root = tempdir("roundtrip");
    let world = root.join("world");
    let store = root.join("store").to_string_lossy().into_owned();
    let region = world.join("region/r.0.0.mca");
    let other = world.join("region/r.1.0.mca");
    write_region(
        &region,
        &[(0, 0, vec![3, 1, 2, 3]), (1, 0, vec![3, 4, 5, 6])],
    );
    write_region(&other, &[(32, 0, vec![3, 7, 7, 7])]);

    let (report, timings) = sekai_app::backup(&world, &store, options(), |_| {})
        .await
        .unwrap();
    assert_eq!(report.chunks, 3);
    assert_eq!(report.new_blobs, 3);
    assert_eq!(timings.regions.len(), 2);

    let snapshots = sekai_app::list_snapshots(&store).await.unwrap();
    assert_eq!(snapshots.len(), 1);

    // Change one chunk, add nothing: the untouched file carries over.
    write_region(
        &region,
        &[(0, 0, vec![3, 9, 9, 9]), (1, 0, vec![3, 4, 5, 6])],
    );
    let (report2, _) = sekai_app::backup(&world, &store, options(), |_| {})
        .await
        .unwrap();
    assert_eq!(report2.new_blobs, 1);
    assert_eq!(report2.carried_chunks, 1);
    assert_eq!(report2.skipped_regions, 1);

    // Roll back to the first snapshot: the changed chunk reverts.
    let (rolled, _rollback_timings) = sekai_app::rollback(&world, &store, snapshots[0].id)
        .await
        .unwrap();
    assert_eq!(rolled.files_written, 2);
    assert_eq!(rolled.chunks_restored, 3);
    let coords = read_coords(&region);
    assert!(coords.contains(&(0, 0)) && coords.contains(&(1, 0)));
    let bytes = std::fs::read(&region).unwrap();
    let image = sekai_anvil::RegionImage::from_bytes(bytes, 0, 0).unwrap();
    let mut payloads = Vec::new();
    image
        .visit_chunks(|chunk| {
            payloads.push(chunk.payload.to_vec());
            true
        })
        .unwrap();
    assert!(payloads.contains(&vec![3, 1, 2, 3]));
    cleanup(&root);
}

#[tokio::test]
async fn strict_rollback_removes_post_snapshot_files() {
    let root = tempdir("strict");
    let world = root.join("world");
    let store = root.join("store").to_string_lossy().into_owned();
    let kept = world.join("region/r.0.0.mca");
    write_region(&kept, &[(0, 0, vec![3, 1])]);

    sekai_app::backup(&world, &store, options(), |_| {})
        .await
        .unwrap();

    // Afterwards: the known chunk vanishes and a new region appears.
    let added = world.join("region/r.1.0.mca");
    write_region(&added, &[(32, 0, vec![3, 2])]);
    write_region(&kept, &[]);
    sekai_app::backup(&world, &store, options(), |_| {})
        .await
        .unwrap();
    let snapshots = sekai_app::list_snapshots(&store).await.unwrap();
    assert_eq!(snapshots.len(), 2);

    // Snapshot 2: all-tombstone region deleted (not shelled), new chunk kept.
    let (rolled, _) = sekai_app::rollback(&world, &store, snapshots[1].id)
        .await
        .unwrap();
    assert_eq!(rolled.files_written, 1);
    assert_eq!(rolled.files_deleted, 1);
    assert_eq!(rolled.chunks_restored, 1);
    assert!(!kept.exists());
    assert_eq!(read_coords(&added), vec![(32, 0)]);

    // Snapshot 1: original chunk rebuilt, post-snapshot file removed.
    let (rolled, _) = sekai_app::rollback(&world, &store, snapshots[0].id)
        .await
        .unwrap();
    assert_eq!(rolled.files_written, 1);
    assert_eq!(rolled.files_deleted, 1);
    assert_eq!(read_coords(&kept), vec![(0, 0)]);
    assert!(!added.exists());
    cleanup(&root);
}

#[tokio::test]
async fn with_diff_records_diff_hashes() {
    use sekai_core::MetaStore as _;
    let root = tempdir("diff");
    let world = root.join("world");
    let store_dir = root.join("store");
    let store_url = store_dir.to_string_lossy().into_owned();
    // Minimal zlib NBT: type byte 2 + zlib("<nbt>") is not valid NBT, so
    // build a real one: uncompressed empty compound root.
    let nbt = vec![10, 0, 0, 0];
    let mut payload = vec![3];
    payload.extend_from_slice(&nbt);
    write_region(&world.join("region/r.0.0.mca"), &[(0, 0, payload)]);

    let options = BackupOptions {
        with_diff: true,
        ..options()
    };
    let (report, _) = sekai_app::backup(&world, &store_url, options, |_| {})
        .await
        .unwrap();
    assert_eq!(report.chunks, 1);

    let snapshots = sekai_app::list_snapshots(&store_url).await.unwrap();
    let meta = sekai_storage::open_sqlite(&store_dir).await.unwrap();
    let row = meta
        .meta()
        .lookup_chunk(
            snapshots[0].id,
            &sekai_core::ChunkCoord::new(
                sekai_core::Dimension::OVERWORLD,
                sekai_core::RegionKind::REGION,
                0,
                0,
            ),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(row.blob.is_some());
    assert!(row.diff.is_some());
    cleanup(&root);
}

#[tokio::test]
async fn progress_fires_per_changed_file() {
    use std::sync::{Arc, Mutex};
    let root = tempdir("progress");
    let world = root.join("world");
    let store = root.join("store").to_string_lossy().into_owned();
    write_region(&world.join("region/r.0.0.mca"), &[(0, 0, vec![3, 1])]);
    write_region(&world.join("region/r.1.0.mca"), &[(32, 0, vec![3, 2])]);

    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&events);
    sekai_app::backup(&world, &store, options(), move |p| {
        seen.lock().unwrap().push((p.files_done, p.files_total));
    })
    .await
    .unwrap();
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|&(_, total)| total == 2));
    assert_eq!(events.last().unwrap().0, 2);
    cleanup(&root);
}

#[tokio::test]
async fn errors_surface_loudly() {
    let root = tempdir("errors");
    let store = root.join("store").to_string_lossy().into_owned();
    // Missing world.
    assert!(
        sekai_app::backup(&root.join("nope"), &store, options(), |_| {})
            .await
            .is_err()
    );
    // Unknown snapshot (empty store is created on open, then lookup fails).
    assert!(
        sekai_app::rollback(&root.join("world"), &store, SnapshotId(99))
            .await
            .is_err()
    );
    cleanup(&root);
}

#[tokio::test]
async fn gc_runs_and_returns_timings() {
    let root = tempdir("gc");
    let world = root.join("world");
    let store = root.join("store").to_string_lossy().into_owned();
    write_region(&world.join("region/r.0.0.mca"), &[(0, 0, vec![3, 1])]);

    sekai_app::backup(&world, &store, options(), |_| {})
        .await
        .unwrap();

    let plan = sekai_app::gc_plan(&store).await.unwrap();
    assert_eq!(plan.len(), 0);

    let (report, timings) = sekai_app::gc(&store).await.unwrap();
    assert_eq!(report.candidates, 0);
    assert_eq!(report.removed, 0);
    assert!(timings.total >= timings.plan + timings.apply);
    cleanup(&root);
}

#[tokio::test]
async fn diff_chunk_between_snapshots() {
    fn build_nbt(status: &str) -> Vec<u8> {
        let mut nbt = vec![10, 0, 0, 8, 0, 6]; // TAG_Compound(""), TAG_String("Status")
        nbt.extend_from_slice(b"Status");
        let len = u16::try_from(status.len()).unwrap();
        nbt.extend_from_slice(&len.to_be_bytes());
        nbt.extend_from_slice(status.as_bytes());
        nbt.push(0); // TAG_End

        let mut payload = vec![3]; // Uncompressed header byte
        payload.extend(nbt);
        payload
    }

    let root = tempdir("diff_chunk");
    let world = root.join("world");
    let store = root.join("store").to_string_lossy().into_owned();
    let region = world.join("region/r.0.0.mca");

    write_region(&region, &[(0, 0, build_nbt("minecraft:full"))]);
    sekai_app::backup(&world, &store, options(), |_| {})
        .await
        .unwrap();

    write_region(&region, &[(0, 0, build_nbt("minecraft:empty"))]);
    sekai_app::backup(&world, &store, options(), |_| {})
        .await
        .unwrap();

    let snapshots = sekai_app::list_snapshots(&store).await.unwrap();
    assert_eq!(snapshots.len(), 2);

    let coord = sekai_core::ChunkCoord::new(
        sekai_core::Dimension::OVERWORLD,
        sekai_core::RegionKind::REGION,
        0,
        0,
    );

    let diffs = sekai_app::diff_chunk(&store, snapshots[0].id, snapshots[1].id, &coord, None)
        .await
        .unwrap();

    assert_eq!(diffs.len(), 1);
    assert_eq!(diffs[0].path, "Status");

    cleanup(&root);
}

#[tokio::test]
async fn diff_world_chunk_with_snapshot() {
    fn build_nbt(status: &str) -> Vec<u8> {
        let mut nbt = vec![10, 0, 0, 8, 0, 6]; // TAG_Compound(""), TAG_String("Status")
        nbt.extend_from_slice(b"Status");
        let len = u16::try_from(status.len()).unwrap();
        nbt.extend_from_slice(&len.to_be_bytes());
        nbt.extend_from_slice(status.as_bytes());
        nbt.push(0); // TAG_End

        let mut payload = vec![3]; // Uncompressed header byte
        payload.extend(nbt);
        payload
    }

    let root = tempdir("diff_world_chunk");
    let world = root.join("world");
    let store = root.join("store").to_string_lossy().into_owned();
    let region = world.join("region/r.0.0.mca");

    write_region(&region, &[(0, 0, build_nbt("minecraft:full"))]);
    sekai_app::backup(&world, &store, options(), |_| {})
        .await
        .unwrap();

    write_region(&region, &[(0, 0, build_nbt("minecraft:empty"))]);

    let coord = sekai_core::ChunkCoord::new(
        sekai_core::Dimension::OVERWORLD,
        sekai_core::RegionKind::REGION,
        0,
        0,
    );

    let diffs = sekai_app::diff_world_chunk(&world, &store, None, &coord, None)
        .await
        .unwrap();

    assert_eq!(diffs.len(), 1);
    assert_eq!(diffs[0].path, "Status");

    cleanup(&root);
}
