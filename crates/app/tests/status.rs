//! Read-only commit previews over real world folders and local stores.
//!
//! Requires the `backend-local` feature.

#![cfg(feature = "backend-local")]

mod fixtures;
mod support;

use fixtures::chunk_nbt;

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

/// Stored blob count, read through the live instance's store handle: the
/// keyspace admits one opener, so a second handle cannot exist.
async fn blob_count(store: &sekai_storage_local::LocalStore) -> usize {
    let mut count = 0usize;
    sekai_storage::BlobStore::visit_blobs(store.blob(), |_| {
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
    write_region(
        &region,
        &[(0, 0, chunk_nbt("marker-1")), (1, 0, chunk_nbt("marker-2"))],
    );

    let mut instance = support::init_store(&store).await.unwrap();
    let (committed, _) = instance
        .world_mut(&mut world)
        .commit(
            sekai_app::CommitOptions::default(),
            sekai_app::Scope::World,
            &(),
        )
        .await
        .unwrap();
    write_region(&region, &[(0, 0, chunk_nbt("marker-1"))]);

    let commits_before = support::all_commits(&instance).await.len();
    let blobs_before = blob_count(instance.store()).await;
    let (report, timings) = instance
        .world(&world)
        .status(options(), sekai_app::Scope::World, &())
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
        timings.total >= timings.discover + timings.universe_load,
        "total timing must cover its phases"
    );
    // A preview is pure: no commit rows and no blobs appear.
    assert_eq!(support::all_commits(&instance).await.len(), commits_before);
    assert_eq!(blob_count(instance.store()).await, blobs_before);
}

#[tokio::test]
async fn status_reports_a_clean_world_after_commit() {
    let (root, _guard) = tempdir("clean");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, chunk_nbt("marker-1")), (1, 0, chunk_nbt("marker-2"))],
    );

    let mut instance = support::init_store(&store).await.unwrap();
    let (committed, _) = instance
        .world_mut(&mut world)
        .commit(
            sekai_app::CommitOptions::default(),
            sekai_app::Scope::World,
            &(),
        )
        .await
        .unwrap();

    let (report, timings) = instance
        .world(&world)
        .status(options(), sekai_app::Scope::World, &())
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
        timings.total >= timings.discover + timings.universe_load,
        "total timing must cover its phases"
    );
}

/// A clean preview answers proven-unchanged files from the committed
/// region nodes: no chunk is visited or hashed (ADR-0029).
#[tokio::test]
async fn status_proves_a_clean_world_without_hashing() {
    let (root, _guard) = tempdir("proof-clean");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, chunk_nbt("marker-1")), (1, 0, chunk_nbt("marker-2"))],
    );

    let mut instance = support::init_store(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .update_cache(sekai_app::CacheUpdateOptions::default(), &())
        .await
        .unwrap();
    instance
        .world_mut(&mut world)
        .commit(
            sekai_app::CommitOptions::default(),
            sekai_app::Scope::World,
            &(),
        )
        .await
        .unwrap();

    let (report, timings) = instance
        .world(&world)
        .status(options(), sekai_app::Scope::World, &())
        .await
        .unwrap();
    assert!(report.clean);
    assert!(
        timings.hash.is_zero(),
        "a proven region must not hash chunks: {:?}",
        timings.hash
    );
    assert!(timings.ingest.is_zero());
    assert!(timings.region_open.is_zero());
}

/// The cache is derived state: wiping it costs performance only. The
/// preview stays clean but falls back to visiting and hashing chunks.
#[tokio::test]
async fn status_wiped_cache_falls_back_to_hashing() {
    let (root, _guard) = tempdir("proof-wiped");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store_dir = root.join("store");
    let store = store_dir.to_string_lossy().into_owned();
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, chunk_nbt("marker-1"))],
    );

    let mut instance = support::init_store(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .update_cache(sekai_app::CacheUpdateOptions::default(), &())
        .await
        .unwrap();
    instance
        .world_mut(&mut world)
        .commit(
            sekai_app::CommitOptions::default(),
            sekai_app::Scope::World,
            &(),
        )
        .await
        .unwrap();
    drop(instance);
    std::fs::remove_dir_all(store_dir.join("cache")).unwrap();
    let instance = support::init_store(&store).await.unwrap();

    let (report, timings) = instance
        .world(&world)
        .status(options(), sekai_app::Scope::World, &())
        .await
        .unwrap();
    assert!(report.clean);
    assert!(
        !timings.hash.is_zero(),
        "a cold cache must fall back to chunk hashing"
    );
}

/// A partial scope still proves the whole file unchanged: only in-scope
/// coordinates are staged, and the out-of-scope chunk must not become a
/// false tombstone.
#[tokio::test]
async fn status_partial_scope_proves_without_false_tombstones() {
    let (root, _guard) = tempdir("proof-partial");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, chunk_nbt("marker-1")), (1, 0, chunk_nbt("marker-2"))],
    );

    let mut instance = support::init_store(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .update_cache(sekai_app::CacheUpdateOptions::default(), &())
        .await
        .unwrap();
    instance
        .world_mut(&mut world)
        .commit(
            sekai_app::CommitOptions::default(),
            sekai_app::Scope::World,
            &(),
        )
        .await
        .unwrap();

    let scope = sekai_app::Scope::Select {
        kinds: sekai_app::Scope::all_kinds(),
        areas: vec![(
            sekai_app::Dimension::OVERWORLD,
            sekai_app::Area::Rect(sekai_app::Rect::new(0, 0, 0, 0)),
        )],
    };
    let (report, timings) = instance
        .world(&world)
        .status(options(), scope, &())
        .await
        .unwrap();
    assert!(report.clean);
    assert_eq!(report.tombstones, 0);
    assert_eq!(report.new_chunks, 0);
    assert!(timings.hash.is_zero());
}

/// A change outside the previewed scope fails the whole-file proof, so
/// the file falls back to ingest - which stages only the in-scope chunk
/// and still reports clean.
#[tokio::test]
async fn status_out_of_scope_change_falls_back_and_stays_clean() {
    let (root, _guard) = tempdir("proof-out-of-scope");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let path = world_path.join("region/r.0.0.mca");
    write_region(
        &path,
        &[(0, 0, chunk_nbt("marker-1")), (1, 0, chunk_nbt("marker-2"))],
    );

    let mut instance = support::init_store(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .update_cache(sekai_app::CacheUpdateOptions::default(), &())
        .await
        .unwrap();
    instance
        .world_mut(&mut world)
        .commit(
            sekai_app::CommitOptions::default(),
            sekai_app::Scope::World,
            &(),
        )
        .await
        .unwrap();
    write_region(
        &path,
        &[(0, 0, chunk_nbt("marker-1")), (1, 0, chunk_nbt("marker-9"))],
    );

    let scope = sekai_app::Scope::Select {
        kinds: sekai_app::Scope::all_kinds(),
        areas: vec![(
            sekai_app::Dimension::OVERWORLD,
            sekai_app::Area::Rect(sekai_app::Rect::new(0, 0, 0, 0)),
        )],
    };
    let (report, timings) = instance
        .world(&world)
        .status(options(), scope, &())
        .await
        .unwrap();
    assert!(report.clean);
    assert_eq!(report.tombstones, 0);
    assert!(
        !timings.hash.is_zero(),
        "a changed file must fall back to chunk hashing"
    );
}
