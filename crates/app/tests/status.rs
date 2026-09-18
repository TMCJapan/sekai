//! Read-only backup previews over real world folders and SQLite stores.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sekai-status-{name}-{}-{}",
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

fn blob_count(store: &Path) -> usize {
    let mut count = 0usize;
    let mut dirs = vec![store.join("blobs")];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                count += 1;
            }
        }
    }
    count
}

const fn options() -> sekai_app::StatusOptions {
    sekai_app::StatusOptions { concurrency: 2 }
}

#[tokio::test]
async fn status_reports_clean_world() {
    let root = tempdir("clean");
    let world = root.join("world");
    let store_dir = root.join("store");
    let store = store_dir.to_string_lossy().into_owned();
    write_region(&world.join("region/r.0.0.mca"), &[(0, 0, vec![3, 1])]);

    sekai_app::backup(
        &world,
        &store,
        sekai_app::BackupOptions::default(),
        sekai_app::Scope::World,
        |_| {},
    )
    .await
    .unwrap();
    let snapshots = sekai_app::list_snapshots(&store).await.unwrap();

    let (report, timings) =
        sekai_app::status(&world, &store, options(), sekai_app::Scope::World, |_| {})
            .await
            .unwrap();
    assert!(report.clean);
    assert_eq!(report.latest, Some(snapshots[0].id));
    assert_eq!(report.changed_regions, 0);
    assert_eq!(report.new_chunks, 0);
    assert!(timings.total >= timings.discover + timings.fingerprint);
    cleanup(&root);
}

#[tokio::test]
async fn status_counts_changes_without_writing() {
    let root = tempdir("dirty");
    let world = root.join("world");
    let store_dir = root.join("store");
    let store = store_dir.to_string_lossy().into_owned();
    write_region(&world.join("region/r.0.0.mca"), &[(0, 0, vec![3, 1])]);

    sekai_app::backup(
        &world,
        &store,
        sekai_app::BackupOptions::default(),
        sekai_app::Scope::World,
        |_| {},
    )
    .await
    .unwrap();
    let blobs_before = blob_count(&store_dir);

    // Diverge: change one chunk, add one, delete the region's sibling file.
    write_region(
        &world.join("region/r.0.0.mca"),
        &[(0, 0, vec![3, 9]), (1, 0, vec![3, 2])],
    );
    write_region(&world.join("region/r.1.0.mca"), &[(32, 0, vec![3, 3])]);

    let (report, _) = sekai_app::status(&world, &store, options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    assert!(!report.clean);
    assert_eq!(report.changed_regions, 2);
    assert_eq!(report.new_files, 1);
    assert_eq!(report.deleted_files, 0);
    assert_eq!(report.new_chunks, 3);
    assert_eq!(report.tombstones, 0);
    assert_eq!(report.new_blobs, 3);

    // Preview wrote nothing: same snapshots, same blobs.
    assert_eq!(sekai_app::list_snapshots(&store).await.unwrap().len(), 1);
    assert_eq!(blob_count(&store_dir), blobs_before);

    // A following backup records exactly what status predicted.
    let (backup, _) = sekai_app::backup(
        &world,
        &store,
        sekai_app::BackupOptions::default(),
        sekai_app::Scope::World,
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(backup.chunks, 3);
    assert_eq!(backup.new_blobs, 3);
    assert_eq!(backup.tombstones, 0);
    cleanup(&root);
}

#[tokio::test]
async fn status_counts_tombstones() {
    let root = tempdir("tomb");
    let world = root.join("world");
    let store = root.join("store").to_string_lossy().into_owned();
    let region = world.join("region/r.0.0.mca");
    write_region(&region, &[(0, 0, vec![3, 1]), (1, 0, vec![3, 2])]);

    sekai_app::backup(
        &world,
        &store,
        sekai_app::BackupOptions::default(),
        sekai_app::Scope::World,
        |_| {},
    )
    .await
    .unwrap();
    write_region(&region, &[(0, 0, vec![3, 1])]);

    let (report, _) = sekai_app::status(&world, &store, options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    assert!(!report.clean);
    // The rewritten region re-ingests its chunk (same blob, fresh row)
    // while the vanished chunk becomes a tombstone.
    assert_eq!(report.new_chunks, 1);
    assert_eq!(report.tombstones, 1);
    assert_eq!(report.new_blobs, 0);
    cleanup(&root);
}
