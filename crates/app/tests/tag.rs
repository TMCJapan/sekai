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
    std::fs::write(path, image).unwrap();
}

#[tokio::test]
async fn tag_create_list_resolve_delete() {
    use sekai_app::TagName;
    let root = tempdir("crud");
    let mut world = root.join("world");
    let store = root.join("store").to_string_lossy().into_owned();
    write_region(world.join("region/r.0.0.mca"), &[(0, 0, vec![3, 1])]);

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .backup(
            sekai_app::BackupOptions::default(),
            sekai_app::Scope::World,
            |_| {},
        )
        .await
        .unwrap();
    let snapshots = instance.list_snapshots().await.unwrap();
    let name = TagName::parse("stable").unwrap();

    let record = instance
        .create_tag(&name, snapshots[0].id, false)
        .await
        .unwrap();
    assert_eq!(record.snapshot, snapshots[0].id);

    // Duplicate without force fails loudly.
    assert!(
        instance
            .create_tag(&name, snapshots[0].id, false)
            .await
            .is_err()
    );

    let tags = instance.list_tags().await.unwrap();
    assert_eq!(tags.len(), 1);

    // Tag refs resolve, unknown refs fail.
    assert_eq!(
        instance.resolve_snapshot_ref("@stable").await.unwrap(),
        snapshots[0].id
    );
    assert_eq!(
        instance.resolve_snapshot_ref("1").await.unwrap(),
        snapshots[0].id
    );
    assert!(instance.resolve_snapshot_ref("@missing").await.is_err());
    assert!(instance.resolve_snapshot_ref("99").await.is_err());
    assert!(instance.resolve_snapshot_ref("nope").await.is_err());

    // Rollback accepts the tag ref end to end.
    let out = root.join("exported");
    let id = instance.resolve_snapshot_ref("@stable").await.unwrap();
    let (report, _) = instance
        .export(
            &out,
            id,
            sekai_app::LayoutFlavor::Legacy,
            sekai_app::ExportOptions::default(),
            sekai_app::Scope::World,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(report.chunks_restored, 1);

    assert!(instance.delete_tag(&name).await.unwrap());
    assert!(!instance.delete_tag(&name).await.unwrap());
    assert!(instance.list_tags().await.unwrap().is_empty());
    cleanup(&root);
}

#[tokio::test]
async fn tag_missing_snapshot_is_an_error() {
    use sekai_app::{SnapshotId, TagName};
    let root = tempdir("missing");
    let store = root.join("store").to_string_lossy().into_owned();
    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut root.join("world"))
        .backup(
            sekai_app::BackupOptions::default(),
            sekai_app::Scope::World,
            |_| {},
        )
        .await
        .unwrap_err();
    // No snapshots exist: tagging fails instead of pointing nowhere.
    assert!(
        instance
            .create_tag(&TagName::parse("x").unwrap(), SnapshotId(1), false)
            .await
            .is_err()
    );
    cleanup(&root);
}
