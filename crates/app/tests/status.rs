//! Read-only commit previews over real world folders and SQLite stores.
//!
//! Requires the `backend-sqlite` feature.

#![cfg(feature = "backend-sqlite")]

use sekai_app::HostWorldTree;
use std::path::Path;

/// A temp dir whose name starts with the calling test's name (tempfile
/// appends a random suffix), removed when its guard is dropped at the end
/// of the test, panics included.
fn tempdir(name: &str) -> (std::path::PathBuf, tempfile::TempDir) {
    let guard = tempfile::Builder::new()
        .prefix(&format!("sekai-status-{name}-"))
        .tempdir()
        .unwrap();
    (guard.path().to_path_buf(), guard)
}

fn write_region(path: impl AsRef<Path>, chunks: &[(i32, i32, Vec<u8>)]) {
    let name = path.as_ref().file_name().unwrap().to_str().unwrap();
    let (rx, rz) = sekai_anvil::parse_region_name(name).unwrap();
    let mut builder = sekai_anvil::RegionBuilder::new(rx, rz, 0).unwrap();
    for (x, z, payload) in chunks {
        builder.stage_chunk(*x, *z, payload).unwrap();
    }
    let image = builder.image().unwrap();
    std::fs::create_dir_all(path.as_ref().parent().unwrap()).unwrap();
    std::fs::write(path.as_ref(), image).unwrap();
}

const fn options() -> sekai_app::StatusOptions {
    sekai_app::StatusOptions { concurrency: 2 }
}

/// Stored blob count, read through a second handle on the same store.
async fn blob_count(store: impl AsRef<Path>) -> usize {
    let opened = sekai_storage_sqlite::open_sqlite(store).await.unwrap();
    let mut count = 0usize;
    sekai_core::BlobStore::visit_blobs(opened.blob(), |_| {
        count += 1;
        true
    })
    .await
    .unwrap();
    count
}

#[tokio::test]
async fn status_counts_tombstones_without_writing() {
    let (root, _guard) = tempdir("tomb");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let region = world_path.join("region/r.0.0.mca");
    write_region(&region, &[(0, 0, vec![3, 1]), (1, 0, vec![3, 2])]);

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    let (committed, _) = instance
        .world_mut(&mut world)
        .commit(
            sekai_app::CommitOptions::default(),
            sekai_app::Scope::World,
            |_| {},
        )
        .await
        .unwrap();
    write_region(&region, &[(0, 0, vec![3, 1])]);

    let commits_before = instance.list_commits().await.unwrap().len();
    let blobs_before = blob_count(&store).await;
    let (report, timings) = instance
        .world(&world)
        .status(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    assert!(!report.clean);
    // The rewritten region's surviving chunk keeps its blob, so no fresh
    // chunk row is staged; the vanished chunk becomes a tombstone.
    assert_eq!(report.new_chunks, 0);
    assert_eq!(report.tombstones, 1);
    assert_eq!(report.new_blobs, 0);
    assert_eq!(report.latest, Some(committed.commit));
    assert!(
        timings.total >= timings.discover + timings.fingerprint,
        "total timing must cover its phases"
    );
    // A preview is pure: no commit rows and no blob files appear.
    assert_eq!(instance.list_commits().await.unwrap().len(), commits_before);
    assert_eq!(blob_count(&store).await, blobs_before);
}

#[tokio::test]
async fn status_reports_a_clean_world_after_commit() {
    let (root, _guard) = tempdir("clean");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, vec![3, 1]), (1, 0, vec![3, 2])],
    );

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    let (committed, _) = instance
        .world_mut(&mut world)
        .commit(
            sekai_app::CommitOptions::default(),
            sekai_app::Scope::World,
            |_| {},
        )
        .await
        .unwrap();

    let (report, timings) = instance
        .world(&world)
        .status(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    assert!(report.clean);
    assert_eq!(report.latest, Some(committed.commit));
    assert_eq!(report.changed_regions, 0);
    assert_eq!(report.new_files, 0);
    assert_eq!(report.deleted_files, 0);
    assert_eq!(report.new_chunks, 0);
    assert_eq!(report.tombstones, 0);
    assert!(
        timings.total >= timings.discover + timings.fingerprint,
        "total timing must cover its phases"
    );
}
