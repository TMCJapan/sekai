//! End-to-end snapshot tag flows over SQLite stores.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sekai-tag-{name}-{}-{}",
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

#[tokio::test]
async fn tag_create_list_resolve_delete() {
    use sekai_app::TagName;
    let root = tempdir("crud");
    let world = root.join("world");
    let store = root.join("store").to_string_lossy().into_owned();
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
    let name = TagName::parse("stable").unwrap();

    let record = sekai_app::create_tag(&store, &name, snapshots[0].id, false)
        .await
        .unwrap();
    assert_eq!(record.snapshot, snapshots[0].id);

    // Duplicate without force fails loudly.
    assert!(
        sekai_app::create_tag(&store, &name, snapshots[0].id, false)
            .await
            .is_err()
    );

    let tags = sekai_app::list_tags(&store).await.unwrap();
    assert_eq!(tags.len(), 1);

    // Tag refs resolve, unknown refs fail.
    assert_eq!(
        sekai_app::resolve_snapshot_ref(&store, "@stable")
            .await
            .unwrap(),
        snapshots[0].id
    );
    assert_eq!(
        sekai_app::resolve_snapshot_ref(&store, "1").await.unwrap(),
        snapshots[0].id
    );
    assert!(
        sekai_app::resolve_snapshot_ref(&store, "@missing")
            .await
            .is_err()
    );
    assert!(sekai_app::resolve_snapshot_ref(&store, "99").await.is_err());
    assert!(
        sekai_app::resolve_snapshot_ref(&store, "nope")
            .await
            .is_err()
    );

    // Rollback accepts the tag ref end to end.
    let out = root.join("exported");
    let (report, _) = sekai_app::export(
        &out,
        &store,
        sekai_app::resolve_snapshot_ref(&store, "@stable")
            .await
            .unwrap(),
        sekai_app::LayoutFlavor::Legacy,
        sekai_app::ExportOptions::default(),
        sekai_app::Scope::World,
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(report.chunks_restored, 1);

    assert!(sekai_app::delete_tag(&store, &name).await.unwrap());
    assert!(!sekai_app::delete_tag(&store, &name).await.unwrap());
    assert!(sekai_app::list_tags(&store).await.unwrap().is_empty());
    cleanup(&root);
}

#[tokio::test]
async fn tag_missing_snapshot_is_an_error() {
    use sekai_app::{SnapshotId, TagName};
    let root = tempdir("missing");
    let store = root.join("store").to_string_lossy().into_owned();
    sekai_app::backup(
        &root.join("world"),
        &store,
        sekai_app::BackupOptions::default(),
        sekai_app::Scope::World,
        |_| {},
    )
    .await
    .unwrap_err();
    // No snapshots exist: tagging fails instead of pointing nowhere.
    assert!(
        sekai_app::create_tag(&store, &TagName::parse("x").unwrap(), SnapshotId(1), false)
            .await
            .is_err()
    );
    cleanup(&root);
}
