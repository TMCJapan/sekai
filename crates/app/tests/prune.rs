//! End-to-end snapshot pruning over real world folders and SQLite stores.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sekai-prune-{name}-{}-{}",
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

fn backup_options() -> sekai_app::BackupOptions {
    sekai_app::BackupOptions {
        concurrency: 2,
        ..sekai_app::BackupOptions::default()
    }
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

#[tokio::test]
async fn prune_folds_and_gc_reclaims() {
    use sekai_app::TagName;
    let root = tempdir("lifecycle");
    let world = root.join("world");
    let store_dir = root.join("store");
    let store = store_dir.to_string_lossy().into_owned();
    let region = world.join("region/r.0.0.mca");

    // Three snapshots with distinct payloads; the oldest is tagged.
    write_region(&region, &[(0, 0, vec![3, 1])]);
    sekai_app::backup(
        &world,
        &store,
        backup_options(),
        sekai_app::Scope::World,
        |_| {},
    )
    .await
    .unwrap();
    write_region(&region, &[(0, 0, vec![3, 2])]);
    sekai_app::backup(
        &world,
        &store,
        backup_options(),
        sekai_app::Scope::World,
        |_| {},
    )
    .await
    .unwrap();
    write_region(&region, &[(0, 0, vec![3, 3])]);
    sekai_app::backup(
        &world,
        &store,
        backup_options(),
        sekai_app::Scope::World,
        |_| {},
    )
    .await
    .unwrap();
    let snapshots = sekai_app::list_snapshots(&store).await.unwrap();
    assert_eq!(snapshots.len(), 3);
    sekai_app::create_tag(
        &store,
        &TagName::parse("doomed").unwrap(),
        snapshots[0].id,
        false,
    )
    .await
    .unwrap();
    assert_eq!(blob_count(&store_dir), 3);

    // Dry-run first: one deletion planned, nothing touched.
    let (plan, plan_timings) = sekai_app::prune_plan(&store, Some(2), None).await.unwrap();
    assert_eq!(plan.delete, [snapshots[0].id]);
    assert_eq!(plan.retained, [snapshots[1].id, snapshots[2].id]);
    assert_eq!(plan_timings.apply, std::time::Duration::ZERO);
    assert_eq!(sekai_app::list_snapshots(&store).await.unwrap().len(), 3);

    let (report, _) = sekai_app::prune(&store, Some(2), None, |_| {})
        .await
        .unwrap();
    assert_eq!(report.pruned, 1);
    assert_eq!(report.rows_folded + report.rows_dropped, 1);
    let remaining = sekai_app::list_snapshots(&store).await.unwrap();
    assert_eq!(remaining.len(), 2);
    // The tag died with its snapshot by cascade.
    assert!(sekai_app::list_tags(&store).await.unwrap().is_empty());

    // Retained snapshots still restore faithfully through fallback.
    let (rolled, _) = sekai_app::rollback(
        &world,
        &store,
        snapshots[1].id,
        sekai_app::RollbackOptions::default(),
        sekai_app::Scope::World,
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(rolled.chunks_restored, 1);

    // Pruning dereferences but never unlinks: GC reclaims exactly one blob.
    assert_eq!(blob_count(&store_dir), 3);
    let (gc_report, _) = sekai_app::gc(&store, |_| {}).await.unwrap();
    assert_eq!(gc_report.removed, 1);
    assert_eq!(blob_count(&store_dir), 2);
    cleanup(&root);
}

#[tokio::test]
async fn prune_before_ref_and_empty_guards() {
    use sekai_app::SnapshotId;
    let root = tempdir("guards");
    let world = root.join("world");
    let store = root.join("store").to_string_lossy().into_owned();
    for payload in [vec![3, 1], vec![3, 2], vec![3, 3]] {
        write_region(&world.join("region/r.0.0.mca"), &[(0, 0, payload)]);
        sekai_app::backup(
            &world,
            &store,
            backup_options(),
            sekai_app::Scope::World,
            |_| {},
        )
        .await
        .unwrap();
    }

    // `--before 2` retains 2 and newer.
    let (report, _) = sekai_app::prune(&store, None, Some(SnapshotId(2)), |_| {})
        .await
        .unwrap();
    assert_eq!(report.pruned, 1);
    assert_eq!(sekai_app::list_snapshots(&store).await.unwrap().len(), 2);

    // Intersected with keep-last: only snapshot 3 survives.
    let (plan, _) = sekai_app::prune_plan(&store, Some(1), Some(SnapshotId(2)))
        .await
        .unwrap();
    assert_eq!(plan.delete, [SnapshotId(2)]);
    assert_eq!(plan.retained, [SnapshotId(3)]);

    // Selections retaining nothing fail loudly instead of wiping the store.
    assert!(
        sekai_app::prune(&store, Some(0), None, |_| {})
            .await
            .is_err()
    );
    assert!(
        sekai_app::prune(&store, None, Some(SnapshotId(99)), |_| {})
            .await
            .is_err()
    );
    assert_eq!(sekai_app::list_snapshots(&store).await.unwrap().len(), 2);
    cleanup(&root);
}
