//! End-to-end restore flows over real world folders and local stores:
//! git-restore semantics (default HEAD source, scoped writes, no-overlay
//! deletion, no history or ref changes) on top of the checkout machinery.

mod fixtures;

use fixtures::chunk_nbt;

use sekai_app::{
    AppError, Area, CommitOptions, Dimension, Rect, RegionKind, RestoreOptions, Revision, Scope,
};
use sekai_world_local::HostWorldTree;
use std::path::Path;

/// A temp dir whose name starts with the calling test's name (tempfile
/// appends a random suffix), removed when its guard is dropped at the end
/// of the test, panics included.
fn tempdir(name: &str) -> (std::path::PathBuf, tempfile::TempDir) {
    let guard = tempfile::Builder::new()
        .prefix(&format!("sekai-app-{name}-"))
        .tempdir()
        .unwrap();
    (guard.path().to_path_buf(), guard)
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

/// Chunk payloads keyed by coordinate, for comparing rebuilt files
/// (restore rewrites layout, so raw bytes differ).
fn chunk_map(path: impl AsRef<Path>) -> std::collections::BTreeMap<(i32, i32), Vec<u8>> {
    let name = path.as_ref().file_name().unwrap().to_str().unwrap();
    let (rx, rz) = sekai_anvil::parse_region_name(name).unwrap();
    let bytes = std::fs::read(path).unwrap();
    let image = sekai_anvil::RegionImage::from_bytes(bytes, rx, rz).unwrap();
    let mut map = std::collections::BTreeMap::new();
    image
        .visit_chunks(|chunk| {
            map.insert((chunk.x, chunk.z), chunk.payload.to_vec());
            true
        })
        .unwrap();
    map
}

const fn options() -> CommitOptions {
    CommitOptions {}
}

/// One overworld region file as a chunk rectangle, all region families.
fn region_scope(rx: i32, rz: i32) -> Scope {
    Scope::Select {
        kinds: vec![RegionKind::Region],
        areas: vec![(Dimension::OVERWORLD, Area::Rect(Rect::region(rx, rz)))],
    }
}

#[tokio::test]
async fn restore_defaults_to_the_active_ref_and_moves_nothing() {
    let (root, _guard) = tempdir("restore-default-source");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let region = world_path.join("region/r.0.0.mca");
    write_region(&region, &[(0, 0, chunk_nbt("marker-1"))]);

    let mut instance = fixtures::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();
    let head = instance.current_commit_id().await.unwrap();

    // Diverge, then restore with no source: HEAD is the default.
    write_region(&region, &[(0, 0, chunk_nbt("marker-9"))]);
    let (report, _) = instance
        .restore(
            &mut world,
            None,
            region_scope(0, 0),
            RestoreOptions::default(),
            &(),
        )
        .await
        .unwrap();
    assert_eq!(report.files_written, 1);
    assert_eq!(report.files_deleted, 0);
    assert_eq!(report.chunks_restored, 1);
    assert_eq!(
        chunk_map(&region).get(&(0, 0)),
        Some(&chunk_nbt("marker-1"))
    );

    // Restore records nothing and moves no ref.
    assert_eq!(instance.list_commits().await.unwrap().len(), 1);
    assert_eq!(instance.current_commit_id().await.unwrap(), head);
}

#[tokio::test]
async fn restore_only_touches_the_selection() {
    let (root, _guard) = tempdir("restore-scoped");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let selected = world_path.join("region/r.0.0.mca");
    let outside = world_path.join("region/r.1.0.mca");
    write_region(&selected, &[(0, 0, chunk_nbt("marker-1"))]);
    write_region(&outside, &[(32, 0, chunk_nbt("marker-2"))]);

    let mut instance = fixtures::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();

    write_region(&selected, &[(0, 0, chunk_nbt("marker-8"))]);
    write_region(&outside, &[(32, 0, chunk_nbt("marker-9"))]);
    let outside_before = std::fs::read(&outside).unwrap();

    let (report, _) = instance
        .restore(
            &mut world,
            None,
            region_scope(0, 0),
            RestoreOptions::default(),
            &(),
        )
        .await
        .unwrap();
    assert_eq!(report.files_written, 1);
    assert_eq!(report.chunks_restored, 1);
    assert_eq!(
        chunk_map(&selected).get(&(0, 0)),
        Some(&chunk_nbt("marker-1"))
    );
    // The out-of-selection file is byte-for-byte untouched.
    assert_eq!(std::fs::read(&outside).unwrap(), outside_before);
}

#[tokio::test]
async fn no_overlay_deletes_source_unknown_files_and_overlay_keeps_them() {
    let (root, _guard) = tempdir("restore-overlay");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let known = world_path.join("region/r.0.0.mca");
    let added = world_path.join("region/r.1.0.mca");
    write_region(&known, &[(0, 0, chunk_nbt("marker-1"))]);

    let mut instance = fixtures::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();
    let source = Revision::parse("HEAD").unwrap();

    // A region created after the source commit: no-overlay (the default)
    // deletes it to match the source exactly.
    write_region(&added, &[(32, 0, chunk_nbt("marker-2"))]);
    let (report, _) = instance
        .restore(
            &mut world,
            Some(&source),
            Scope::dimension(Dimension::OVERWORLD),
            RestoreOptions::default(),
            &(),
        )
        .await
        .unwrap();
    assert_eq!(report.files_deleted, 1);
    assert!(!added.exists());

    // Overlay mode never removes files.
    write_region(&added, &[(32, 0, chunk_nbt("marker-2"))]);
    let overlay = RestoreOptions {
        overlay: true,
        ..RestoreOptions::default()
    };
    let (report, _) = instance
        .restore(
            &mut world,
            Some(&source),
            Scope::dimension(Dimension::OVERWORLD),
            overlay,
            &(),
        )
        .await
        .unwrap();
    assert_eq!(report.files_deleted, 0);
    assert!(added.exists());
}

#[tokio::test]
async fn restore_replaces_file_content_wholesale_even_in_overlay() {
    let (root, _guard) = tempdir("restore-wholesale");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let region = world_path.join("region/r.0.0.mca");
    write_region(&region, &[(0, 0, chunk_nbt("marker-1"))]);

    let mut instance = fixtures::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();

    // A chunk created after the commit inside a known region: git restore
    // overwrites file content, so the chunk vanishes even under overlay.
    write_region(
        &region,
        &[(0, 0, chunk_nbt("marker-1")), (1, 0, chunk_nbt("marker-2"))],
    );
    let overlay = RestoreOptions {
        overlay: true,
        ..RestoreOptions::default()
    };
    let (report, _) = instance
        .restore(&mut world, None, region_scope(0, 0), overlay, &())
        .await
        .unwrap();
    assert_eq!(report.chunks_restored, 1);
    let map = chunk_map(&region);
    assert_eq!(map.get(&(0, 0)), Some(&chunk_nbt("marker-1")));
    assert!(!map.contains_key(&(1, 0)));
}

#[tokio::test]
async fn restore_from_an_explicit_source_leaves_head_alone() {
    let (root, _guard) = tempdir("restore-explicit-source");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let region = world_path.join("region/r.0.0.mca");
    write_region(&region, &[(0, 0, chunk_nbt("marker-1"))]);

    let mut instance = fixtures::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();
    write_region(&region, &[(0, 0, chunk_nbt("marker-2"))]);
    instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();
    let commits = instance.list_commits().await.unwrap();
    assert_eq!(commits.len(), 2);

    // `HEAD~1` names the older commit; the active ref stays on the newer.
    let older = Revision::parse("HEAD~1").unwrap();
    instance
        .restore(
            &mut world,
            Some(&older),
            region_scope(0, 0),
            RestoreOptions::default(),
            &(),
        )
        .await
        .unwrap();
    assert_eq!(
        chunk_map(&region).get(&(0, 0)),
        Some(&chunk_nbt("marker-1"))
    );
    assert_eq!(
        instance.current_commit_id().await.unwrap(),
        Some(commits[1].id)
    );

    // A commit ID spelling works the same way.
    let newer = Revision::parse(&commits[1].id.to_string()).unwrap();
    instance
        .restore(
            &mut world,
            Some(&newer),
            region_scope(0, 0),
            RestoreOptions::default(),
            &(),
        )
        .await
        .unwrap();
    assert_eq!(
        chunk_map(&region).get(&(0, 0)),
        Some(&chunk_nbt("marker-2"))
    );
    assert_eq!(instance.list_commits().await.unwrap().len(), 2);
}

#[tokio::test]
async fn restore_recreates_a_deleted_file() {
    let (root, _guard) = tempdir("restore-deleted-file");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let region = world_path.join("region/r.0.0.mca");
    write_region(&region, &[(0, 0, chunk_nbt("marker-1"))]);

    let mut instance = fixtures::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();

    std::fs::remove_file(&region).unwrap();
    let (report, _) = instance
        .restore(
            &mut world,
            None,
            region_scope(0, 0),
            RestoreOptions::default(),
            &(),
        )
        .await
        .unwrap();
    assert_eq!(report.files_written, 1);
    assert_eq!(
        chunk_map(&region).get(&(0, 0)),
        Some(&chunk_nbt("marker-1"))
    );
}

#[tokio::test]
async fn restore_fails_without_a_source_commit() {
    let (root, _guard) = tempdir("restore-unborn");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, chunk_nbt("marker-1"))],
    );

    let mut instance = fixtures::init(&store).await.unwrap();
    // Unborn HEAD: no default source exists.
    let err = instance
        .restore(
            &mut world,
            None,
            region_scope(0, 0),
            RestoreOptions::default(),
            &(),
        )
        .await
        .expect_err("an unborn head has nothing to restore from");
    assert!(matches!(err, AppError::NoCommits));

    // An unknown explicit source fails as a revision error.
    let unknown = Revision::parse(&"ab".repeat(32)).unwrap();
    assert!(
        instance
            .restore(
                &mut world,
                Some(&unknown),
                region_scope(0, 0),
                RestoreOptions::default(),
                &(),
            )
            .await
            .is_err()
    );
}
