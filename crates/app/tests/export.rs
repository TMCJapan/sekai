//! End-to-end snapshot exports into fresh directories.

use sekai_app::HostWorldTree;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sekai-export-{name}-{}-{}",
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

/// Write a region image through the real builder, deriving the region
/// coordinates from the file name.
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

/// Chunk payloads keyed by coordinate.
fn chunk_map(path: impl AsRef<Path>) -> BTreeMap<(i32, i32), Vec<u8>> {
    let name = path.as_ref().file_name().unwrap().to_str().unwrap();
    let (rx, rz) = sekai_anvil::parse_region_name(name).unwrap();
    let bytes = std::fs::read(path).unwrap();
    let image = sekai_anvil::RegionImage::from_bytes(bytes, rx, rz).unwrap();
    let mut map = BTreeMap::new();
    image
        .visit_chunks(|chunk| {
            map.insert((chunk.x, chunk.z), chunk.payload.to_vec());
            true
        })
        .unwrap();
    map
}

fn backup_options() -> sekai_app::BackupOptions {
    sekai_app::BackupOptions {
        concurrency: 2,
        ..sekai_app::BackupOptions::default()
    }
}

#[tokio::test]
async fn export_rebuilds_snapshot_into_fresh_directory() {
    use sekai_app::LayoutFlavor;
    let root = tempdir("roundtrip");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path);
    let store = root.join("store").to_string_lossy().into_owned();
    let over = world_path.join("region/r.0.0.mca");
    let nether = world_path.join("DIM-1/region/r.0.0.mca");
    write_region(&over, &[(0, 0, vec![3, 1]), (1, 0, vec![3, 2])]);
    write_region(&nether, &[(0, 0, vec![3, 3])]);

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .backup(backup_options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    let snapshots = instance.list_snapshots().await.unwrap();

    // Diverge live afterwards: export must still reproduce the snapshot.
    write_region(&over, &[(0, 0, vec![3, 9])]);

    let out_path = root.join("exported");
    let out = HostWorldTree::new(&out_path);
    let (report, timings) = instance
        .export(
            &out,
            snapshots[0].id,
            LayoutFlavor::Legacy,
            sekai_app::ExportOptions::default(),
            sekai_app::Scope::World,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(report.files_written, 2);
    assert_eq!(report.chunks_restored, 3);
    assert!(timings.total >= timings.plan + timings.export_files);

    let mut expected = BTreeMap::new();
    expected.insert((0, 0), vec![3, 1]);
    expected.insert((1, 0), vec![3, 2]);
    assert_eq!(chunk_map(out_path.join("region/r.0.0.mca")), expected);
    let mut expected_nether = BTreeMap::new();
    expected_nether.insert((0, 0), vec![3, 3]);
    assert_eq!(
        chunk_map(out_path.join("DIM-1/region/r.0.0.mca")),
        expected_nether
    );
    cleanup(&root);
}

#[tokio::test]
async fn export_honors_layout_flavor_and_scope() {
    use sekai_app::{Dimension, LayoutFlavor, Scope};
    let root = tempdir("flavor-scope");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path);
    let store = root.join("store").to_string_lossy().into_owned();
    write_region(world_path.join("region/r.0.0.mca"), &[(0, 0, vec![3, 1])]);
    write_region(
        world_path.join("DIM-1/region/r.0.0.mca"),
        &[(0, 0, vec![3, 2])],
    );

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .backup(backup_options(), Scope::World, |_| {})
        .await
        .unwrap();
    let snapshots = instance.list_snapshots().await.unwrap();

    // Modern layout, overworld scope only: the nether file stays out.
    let out_path = root.join("modern");
    let out = HostWorldTree::new(&out_path);
    let (report, _) = instance
        .export(
            &out,
            snapshots[0].id,
            LayoutFlavor::New,
            sekai_app::ExportOptions::default(),
            Scope::dimension(Dimension::OVERWORLD),
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(report.files_written, 1);
    assert_eq!(report.chunks_restored, 1);
    assert!(
        out_path
            .join("dimensions/minecraft/overworld/region/r.0.0.mca")
            .is_file()
    );
    assert!(
        !out_path
            .join("dimensions")
            .join("minecraft/the_nether")
            .exists()
    );

    // Bukkit layout, whole world.
    let bukkit_path = root.join("bukkit");
    let bukkit = HostWorldTree::new(&bukkit_path);
    let (report, _) = instance
        .export(
            &bukkit,
            snapshots[0].id,
            LayoutFlavor::Bukkit {
                base: "myworld".to_owned(),
            },
            sekai_app::ExportOptions::default(),
            Scope::World,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(report.files_written, 2);
    assert!(bukkit_path.join("myworld/region/r.0.0.mca").is_file());
    assert!(
        bukkit_path
            .join("myworld_nether/DIM-1/region/r.0.0.mca")
            .is_file()
    );
    cleanup(&root);
}

#[tokio::test]
async fn export_refuses_non_empty_directory() {
    use sekai_app::LayoutFlavor;
    let root = tempdir("nonempty");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path);
    let store = root.join("store").to_string_lossy().into_owned();
    write_region(world_path.join("region/r.0.0.mca"), &[(0, 0, vec![3, 1])]);

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .backup(backup_options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    let snapshots = instance.list_snapshots().await.unwrap();

    let out_path = root.join("out");
    let out = HostWorldTree::new(&out_path);
    std::fs::create_dir_all(&out_path).unwrap();
    std::fs::write(out_path.join("existing.txt"), b"data").unwrap();
    assert!(
        instance
            .export(
                &out,
                snapshots[0].id,
                LayoutFlavor::Legacy,
                sekai_app::ExportOptions::default(),
                sekai_app::Scope::World,
                |_| {},
            )
            .await
            .is_err()
    );
    // An empty directory is accepted.
    std::fs::remove_file(out_path.join("existing.txt")).unwrap();
    let (report, _) = instance
        .export(
            &out,
            snapshots[0].id,
            LayoutFlavor::Legacy,
            sekai_app::ExportOptions::default(),
            sekai_app::Scope::World,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(report.files_written, 1);
    cleanup(&root);
}

#[tokio::test]
async fn export_omits_tombstoned_regions() {
    use sekai_app::LayoutFlavor;
    let root = tempdir("tombstones");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path);
    let store = root.join("store").to_string_lossy().into_owned();
    let region = world_path.join("region/r.0.0.mca");
    write_region(&region, &[(0, 0, vec![3, 1])]);

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .backup(backup_options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    // Empty the region: the second snapshot tombstones its only chunk.
    write_region(&region, &[]);
    instance
        .world_mut(&mut world)
        .backup(backup_options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    let snapshots = instance.list_snapshots().await.unwrap();

    let out_path = root.join("out");
    let out = HostWorldTree::new(&out_path);
    let (report, _) = instance
        .export(
            &out,
            snapshots[1].id,
            LayoutFlavor::Legacy,
            sekai_app::ExportOptions::default(),
            sekai_app::Scope::World,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(report.files_written, 0);
    assert_eq!(report.chunks_restored, 0);
    assert!(!out_path.join("region/r.0.0.mca").exists());
    cleanup(&root);
}
