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

    let (report, timings) =
        sekai_app::backup(&world, &store, options(), sekai_app::Scope::World, |_| {})
            .await
            .unwrap();
    assert_eq!(report.chunks, 3);
    assert_eq!(report.new_blobs, 3);
    assert_eq!(timings.regions.len(), 2);

    let snapshots = sekai_app::list_snapshots(&store).await.unwrap();
    assert_eq!(snapshots.len(), 1);

    // Change one chunk, add nothing: the untouched file carries over.
    // (Tick separation as below: same-size rewrite, millisecond mtime.)
    std::thread::sleep(std::time::Duration::from_millis(5));
    write_region(
        &region,
        &[(0, 0, vec![3, 9, 9, 9]), (1, 0, vec![3, 4, 5, 6])],
    );
    let (report2, _) =
        sekai_app::backup(&world, &store, options(), sekai_app::Scope::World, |_| {})
            .await
            .unwrap();
    assert_eq!(report2.new_blobs, 1);
    assert_eq!(report2.carried_chunks, 1);
    assert_eq!(report2.skipped_regions, 1);

    // Roll back to the first snapshot: the changed chunk reverts.
    let (rolled, _rollback_timings) =
        sekai_app::rollback(&world, &store, snapshots[0].id, sekai_app::Scope::World)
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
async fn scoped_backup_records_no_spurious_tombstones() {
    use sekai_app::{ChunkCoord, Dimension, RegionKind, Scope};
    let root = tempdir("scoped");
    let world = root.join("world");
    let store = root.join("store").to_string_lossy().into_owned();
    let over = world.join("region/r.0.0.mca");
    let nether = world.join("DIM-1/region/r.0.0.mca");
    // Minimal valid NBT (uncompressed empty compound) so chunk diffs parse.
    let payload = vec![3, 10, 0, 0, 0];
    write_region(&over, &[(0, 0, payload.clone()), (1, 0, payload.clone())]);
    write_region(&nether, &[(0, 0, payload.clone())]);

    let (full, _) = sekai_app::backup(&world, &store, options(), Scope::World, |_| {})
        .await
        .unwrap();
    assert_eq!(full.chunks, 3);
    assert_eq!(full.tombstones, 0);

    // Overworld-only backup with no changes: nothing ingested, and the
    // out-of-scope nether chunk must not become a tombstone.
    let (scoped, _) = sekai_app::backup(
        &world,
        &store,
        options(),
        Scope::dimension(Dimension::OVERWORLD),
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(scoped.tombstones, 0);
    assert_eq!(scoped.chunks, 2);

    // The nether chunk still resolves through fallback to the full backup.
    let snapshots = sekai_app::list_snapshots(&store).await.unwrap();
    assert_eq!(snapshots.len(), 2);
    let nether_coord = ChunkCoord::new(Dimension::NETHER, RegionKind::REGION, 0, 0);
    let diffs = sekai_app::diff_chunk(
        &store,
        snapshots[0].id,
        snapshots[1].id,
        &nether_coord,
        None,
    )
    .await
    .unwrap();
    assert!(diffs.is_empty());

    // Vanish one in-scope chunk: exactly that tombstone is recorded.
    write_region(&over, &[(0, 0, payload)]);
    let (scoped2, _) = sekai_app::backup(
        &world,
        &store,
        options(),
        Scope::dimension(Dimension::OVERWORLD),
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(scoped2.tombstones, 1);

    // A following full backup sees no further changes: zero tombstones,
    // zero new blobs, nether chunk intact.
    let (full2, _) = sekai_app::backup(&world, &store, options(), Scope::World, |_| {})
        .await
        .unwrap();
    assert_eq!(full2.tombstones, 0);
    assert_eq!(full2.new_blobs, 0);
    let snapshots = sekai_app::list_snapshots(&store).await.unwrap();
    assert_eq!(snapshots.len(), 4);
    let diffs = sekai_app::diff_chunk(
        &store,
        snapshots[0].id,
        snapshots[3].id,
        &nether_coord,
        None,
    )
    .await
    .unwrap();
    assert!(diffs.is_empty());
    cleanup(&root);
}

#[tokio::test]
async fn scoped_rollback_leaves_other_dimensions_untouched() {
    use sekai_app::{Dimension, Scope};
    let root = tempdir("scoped-rollback");
    let world = root.join("world");
    let store = root.join("store").to_string_lossy().into_owned();
    let over = world.join("region/r.0.0.mca");
    let nether = world.join("DIM-1/region/r.0.0.mca");
    write_region(&over, &[(0, 0, vec![3, 1])]);
    write_region(&nether, &[(0, 0, vec![3, 2])]);

    sekai_app::backup(&world, &store, options(), Scope::World, |_| {})
        .await
        .unwrap();
    let snapshots = sekai_app::list_snapshots(&store).await.unwrap();

    // Diverge both dimensions, then delete the nether file outright.
    write_region(&over, &[(0, 0, vec![3, 9])]);
    write_region(&nether, &[(0, 0, vec![3, 8])]);
    let nether_diverged = std::fs::read(&nether).unwrap();
    std::fs::remove_file(&nether).unwrap();

    // Overworld-scoped rollback: the overworld reverts, the nether file
    // is neither recreated nor deleted (it stays absent), and the report
    // counts only in-scope work.
    let (rolled, _) = sekai_app::rollback(
        &world,
        &store,
        snapshots[0].id,
        Scope::dimension(Dimension::OVERWORLD),
    )
    .await
    .unwrap();
    assert_eq!(rolled.files_written, 1);
    assert_eq!(rolled.files_deleted, 0);
    assert_eq!(rolled.chunks_restored, 1);
    assert_eq!(read_coords(&over), vec![(0, 0)]);
    assert!(!nether.exists());

    // Nether-scoped rollback recreates the deleted file from CAS.
    let (rolled, _) = sekai_app::rollback(
        &world,
        &store,
        snapshots[0].id,
        Scope::dimension(Dimension::NETHER),
    )
    .await
    .unwrap();
    assert_eq!(rolled.files_written, 1);
    assert_eq!(rolled.chunks_restored, 1);
    assert_eq!(read_coords(&nether), vec![(0, 0)]);

    // Overworld-scoped rollback never rewrites an in-place nether file.
    write_region(&nether, &[(0, 0, vec![3, 8])]);
    let before = std::fs::read(&nether).unwrap();
    assert_eq!(before, nether_diverged);
    sekai_app::rollback(
        &world,
        &store,
        snapshots[0].id,
        Scope::dimension(Dimension::OVERWORLD),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&nether).unwrap(), before);
    cleanup(&root);
}

#[tokio::test]
async fn strict_rollback_removes_post_snapshot_files() {
    let root = tempdir("strict");
    let world = root.join("world");
    let store = root.join("store").to_string_lossy().into_owned();
    let kept = world.join("region/r.0.0.mca");
    write_region(&kept, &[(0, 0, vec![3, 1])]);

    sekai_app::backup(&world, &store, options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();

    // Afterwards: the known chunk vanishes and a new region appears.
    let added = world.join("region/r.1.0.mca");
    write_region(&added, &[(32, 0, vec![3, 2])]);
    write_region(&kept, &[]);
    sekai_app::backup(&world, &store, options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    let snapshots = sekai_app::list_snapshots(&store).await.unwrap();
    assert_eq!(snapshots.len(), 2);

    // Snapshot 2: all-tombstone region deleted (not shelled), new chunk kept.
    let (rolled, _) = sekai_app::rollback(&world, &store, snapshots[1].id, sekai_app::Scope::World)
        .await
        .unwrap();
    assert_eq!(rolled.files_written, 1);
    assert_eq!(rolled.files_deleted, 1);
    assert_eq!(rolled.chunks_restored, 1);
    assert!(!kept.exists());
    assert_eq!(read_coords(&added), vec![(32, 0)]);

    // Snapshot 1: original chunk rebuilt, post-snapshot file removed.
    let (rolled, _) = sekai_app::rollback(&world, &store, snapshots[0].id, sekai_app::Scope::World)
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
    let (report, _) =
        sekai_app::backup(&world, &store_url, options, sekai_app::Scope::World, |_| {})
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
    sekai_app::backup(
        &world,
        &store,
        options(),
        sekai_app::Scope::World,
        move |p| {
            seen.lock().unwrap().push((p.files_done, p.files_total));
        },
    )
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
        sekai_app::backup(
            &root.join("nope"),
            &store,
            options(),
            sekai_app::Scope::World,
            |_| {}
        )
        .await
        .is_err()
    );
    // Unknown snapshot (empty store is created on open, then lookup fails).
    assert!(
        sekai_app::rollback(
            &root.join("world"),
            &store,
            SnapshotId(99),
            sekai_app::Scope::World
        )
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

    sekai_app::backup(&world, &store, options(), sekai_app::Scope::World, |_| {})
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
    sekai_app::backup(&world, &store, options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();

    // Same millisecond-tick hazard as below: separate ticks so the
    // same-size rewrite is observed instead of carried.
    std::thread::sleep(std::time::Duration::from_millis(5));
    write_region(&region, &[(0, 0, build_nbt("minecraft:empty"))]);
    sekai_app::backup(&world, &store, options(), sekai_app::Scope::World, |_| {})
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
    sekai_app::backup(&world, &store, options(), sekai_app::Scope::World, |_| {})
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

#[tokio::test]
async fn backs_up_region_with_trailing_partial_sector() {
    // Real files can carry a torn tail (e.g. 1034 sectors + 188 bytes)
    // that vanilla opens fine; backup must accept it as long as every
    // referenced run is intact.
    let root = tempdir("partial-tail");
    let world = root.join("world");
    let store = root.join("store").to_string_lossy().into_owned();
    let region = world.join("region/r.0.0.mca");
    write_region(&region, &[(0, 0, vec![3, 1])]);

    let mut bytes = std::fs::read(&region).unwrap();
    bytes.extend_from_slice(&[0xAB; 188]);
    assert_ne!(bytes.len() % 4096, 0);
    std::fs::write(&region, &bytes).unwrap();

    let (report, _) = sekai_app::backup(&world, &store, options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    assert_eq!(report.chunks, 1);
    assert_eq!(report.new_blobs, 1);
    assert_eq!(read_coords(&region), vec![(0, 0)]);
    cleanup(&root);
}

#[tokio::test]
async fn corrupt_region_names_its_file() {
    // Genuine corruption still fails loudly, now naming the file.
    let root = tempdir("corrupt-names-file");
    let world = root.join("world");
    let store = root.join("store").to_string_lossy().into_owned();
    let region = world.join("region/r.0.0.mca");
    write_region(&region, &[(0, 0, vec![3, 1])]);

    // Point the only entry far past end of file.
    let mut bytes = std::fs::read(&region).unwrap();
    bytes[0..4].copy_from_slice(&((9u32 << 8 | 1).to_be_bytes()));
    std::fs::write(&region, &bytes).unwrap();

    let err = sekai_app::backup(&world, &store, options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap_err();
    let message = format!("{err}");
    assert!(
        message.contains("r.0.0.mca"),
        "error names the file: {message}"
    );
    cleanup(&root);
}

fn build_status_nbt(status: &str) -> Vec<u8> {
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

fn kind_dir(kind: sekai_app::RegionKind) -> &'static str {
    if kind == sekai_app::RegionKind::ENTITIES {
        "entities"
    } else if kind == sekai_app::RegionKind::POI {
        "poi"
    } else {
        "region"
    }
}

#[tokio::test]
async fn sparse_entities_chunk_diffs_empty_without_error() {
    use sekai_app::{ChunkCoord, Dimension, RegionKind};
    let root = tempdir("sparse-entities");
    let world = root.join("world");
    let store = root.join("store").to_string_lossy().into_owned();
    // Dense terrain only; no entities/poi files at all.
    write_region(
        &world.join("region/r.0.0.mca"),
        &[(0, 0, build_status_nbt("minecraft:full"))],
    );

    sekai_app::backup(&world, &store, options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    let snapshots = sekai_app::list_snapshots(&store).await.unwrap();

    // The sparse kinds have no (0,0): previously a loud error, now empty.
    for kind in [RegionKind::ENTITIES, RegionKind::POI] {
        let coord = ChunkCoord::new(Dimension::OVERWORLD, kind, 0, 0);
        let diffs = sekai_app::diff_chunk(&store, snapshots[0].id, snapshots[0].id, &coord, None)
            .await
            .unwrap();
        assert!(diffs.is_empty());
        let world_diffs = sekai_app::diff_world_chunk(&world, &store, None, &coord, None)
            .await
            .unwrap();
        assert!(world_diffs.is_empty());
    }
    cleanup(&root);
}

#[tokio::test]
async fn created_and_deleted_chunks_diff_as_added_removed() {
    use sekai_app::{ChunkCoord, Dimension, RegionKind};
    let root = tempdir("created-deleted");
    let world = root.join("world");
    let store = root.join("store").to_string_lossy().into_owned();
    let entities = world.join("entities/r.0.0.mca");
    let coord = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::ENTITIES, 0, 0);

    // S1: no entities chunk.
    write_region(
        &world.join("region/r.0.0.mca"),
        &[(0, 0, build_status_nbt("minecraft:full"))],
    );
    sekai_app::backup(&world, &store, options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();

    // S2: entities chunk appears.
    write_region(&entities, &[(0, 0, build_status_nbt("minecraft:full"))]);
    sekai_app::backup(&world, &store, options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    let snapshots = sekai_app::list_snapshots(&store).await.unwrap();

    let diffs = sekai_app::diff_chunk(&store, snapshots[0].id, snapshots[1].id, &coord, None)
        .await
        .unwrap();
    assert!(!diffs.is_empty());
    assert!(
        diffs
            .iter()
            .all(|entry| matches!(entry.change, sekai_core::NbtChange::Added(_)))
    );

    // S3: entities chunk vanishes again.
    std::fs::remove_file(&entities).unwrap();
    sekai_app::backup(&world, &store, options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    let snapshots = sekai_app::list_snapshots(&store).await.unwrap();
    let diffs = sekai_app::diff_chunk(&store, snapshots[1].id, snapshots[2].id, &coord, None)
        .await
        .unwrap();
    assert!(!diffs.is_empty());
    assert!(
        diffs
            .iter()
            .all(|entry| matches!(entry.change, sekai_core::NbtChange::Removed(_)))
    );
    cleanup(&root);
}

#[tokio::test]
async fn all_kinds_round_trip() {
    use sekai_app::{ChunkCoord, Dimension, RegionKind};
    for kind in [RegionKind::REGION, RegionKind::ENTITIES, RegionKind::POI] {
        let root = tempdir(&format!("kind-{}", kind_dir(kind)));
        let world = root.join("world");
        let store = root.join("store").to_string_lossy().into_owned();
        let file = world.join(format!("{}/r.0.0.mca", kind_dir(kind)));
        let coord = ChunkCoord::new(Dimension::OVERWORLD, kind, 0, 0);

        write_region(&file, &[(0, 0, build_status_nbt("minecraft:full"))]);
        sekai_app::backup(&world, &store, options(), sekai_app::Scope::World, |_| {})
            .await
            .unwrap();

        // Fingerprints compare mtime at millisecond resolution; tiny worlds
        // back up within one tick, and same-size/same-header files would
        // wrongly carry. Separate the ticks so the rewrite is observed.
        std::thread::sleep(std::time::Duration::from_millis(5));
        write_region(&file, &[(0, 0, build_status_nbt("minecraft:empty"))]);
        sekai_app::backup(&world, &store, options(), sekai_app::Scope::World, |_| {})
            .await
            .unwrap();
        let snapshots = sekai_app::list_snapshots(&store).await.unwrap();

        let diffs = sekai_app::diff_chunk(&store, snapshots[0].id, snapshots[1].id, &coord, None)
            .await
            .unwrap();
        assert_eq!(diffs.len(), 1, "kind {}", kind_dir(kind));
        assert_eq!(diffs[0].path, "Status");

        let (rolled, _) =
            sekai_app::rollback(&world, &store, snapshots[0].id, sekai_app::Scope::World)
                .await
                .unwrap();
        assert_eq!(rolled.chunks_restored, 1);
        cleanup(&root);
    }
}

#[tokio::test]
async fn rect_and_kind_scopes_limit_ingest() {
    use sekai_app::{Area, Dimension, Rect, RegionKind, Scope};
    let root = tempdir("rect-kind");
    let world = root.join("world");
    let store = root.join("store").to_string_lossy().into_owned();
    write_region(
        &world.join("region/r.0.0.mca"),
        &[(0, 0, build_status_nbt("a")), (1, 0, build_status_nbt("b"))],
    );
    write_region(
        &world.join("region/r.1.0.mca"),
        &[(32, 0, build_status_nbt("c"))],
    );
    write_region(
        &world.join("entities/r.0.0.mca"),
        &[(0, 0, build_status_nbt("d"))],
    );

    sekai_app::backup(&world, &store, options(), Scope::World, |_| {})
        .await
        .unwrap();

    // Rectangle over (0,0)-(1,0), region kind only: (32,0) and entities
    // stay out with no tombstones.
    let rect = Scope::Select {
        kinds: vec![RegionKind::REGION],
        areas: vec![(Dimension::OVERWORLD, Area::Rect(Rect::new(0, 0, 1, 0)))],
    };
    let (report, _) = sekai_app::backup(&world, &store, options(), rect, |_| {})
        .await
        .unwrap();
    assert_eq!(report.chunks, 2);
    assert_eq!(report.tombstones, 0);

    // Kind filter alone: entities across the whole overworld.
    let entities = Scope::Select {
        kinds: vec![RegionKind::ENTITIES],
        areas: vec![(Dimension::OVERWORLD, Area::All)],
    };
    let (report, _) = sekai_app::backup(&world, &store, options(), entities, |_| {})
        .await
        .unwrap();
    assert_eq!(report.chunks, 1);
    assert_eq!(report.tombstones, 0);

    // A following full backup is quiet: nothing spurious was recorded.
    let (full, _) = sekai_app::backup(&world, &store, options(), Scope::World, |_| {})
        .await
        .unwrap();
    assert_eq!(full.tombstones, 0);
    assert_eq!(full.new_blobs, 0);
    cleanup(&root);
}
