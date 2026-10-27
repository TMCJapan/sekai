//! Read-only commit previews over real world folders and SQLite stores.
//!
//! Requires the `backend-sqlite` feature.

#![cfg(feature = "backend-sqlite")]

use sekai_app::HostWorldTree;
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

fn cleanup(dir: impl AsRef<Path>) {
    let _ = std::fs::remove_dir_all(dir);
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

#[tokio::test]
async fn status_counts_tombstones() {
    let root = tempdir("tomb");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let region = world_path.join("region/r.0.0.mca");
    write_region(&region, &[(0, 0, vec![3, 1]), (1, 0, vec![3, 2])]);

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(
            sekai_app::CommitOptions::default(),
            sekai_app::Scope::World,
            |_| {},
        )
        .await
        .unwrap();
    write_region(&region, &[(0, 0, vec![3, 1])]);

    let (report, _) = instance
        .world(&world)
        .status(options(), sekai_app::Scope::World, |_| {})
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
