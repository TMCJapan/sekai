//! End-to-end commit flows over real world folders and SQLite stores.
//!
//! Requires the `backend-sqlite` feature.

#![cfg(feature = "backend-sqlite")]

use sekai_app::{CommitId, CommitOptions, RegionKind, Scope};
use sekai_app::{HostWorldTree, WorldTree};
use sekai_nbt::ChunkPayloadDiff;
use sekai_nbt::nanonbt::Cesu8;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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

/// Chunk coordinates present in the region file at `path`.
fn read_coords(path: impl AsRef<Path>) -> Vec<(i32, i32)> {
    let name = path.as_ref().file_name().unwrap().to_str().unwrap();
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

/// Chunk payloads keyed by coordinate, for comparing rebuilt files
/// (checkout rewrites layout, so raw bytes differ).
type ChunkMap = std::collections::BTreeMap<(i32, i32), Vec<u8>>;

fn chunk_map(path: impl AsRef<Path>) -> ChunkMap {
    let name = path.as_ref().file_name().unwrap().to_str().unwrap();
    let (rx, rz) = sekai_anvil::parse_region_name(name).unwrap();
    let bytes = std::fs::read(path).unwrap();
    let image = sekai_anvil::RegionImage::from_bytes(bytes, rx, rz).unwrap();
    let mut map = ChunkMap::new();
    image
        .visit_chunks(|chunk| {
            map.insert((chunk.x, chunk.z), chunk.payload.to_vec());
            true
        })
        .unwrap();
    map
}

fn options() -> CommitOptions {
    CommitOptions {
        concurrency: 2,
        ..CommitOptions::default()
    }
}

/// A parseable but non-canonical file name (`r.00.00.mca`) must be read
/// from the path discovery found, not from the canonical `r.0.0.mca`.
#[tokio::test]
async fn commits_a_non_canonical_region_name() {
    let (root, _guard) = tempdir("non-canonical-name");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    write_region(
        world_path.join("region/r.00.00.mca"),
        &[(0, 0, vec![3, 1, 2, 3])],
    );

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    let (report, timings) = instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    assert_eq!(report.chunks, 1);
    assert_eq!(timings.regions.len(), 1);
    // Component-wise joins so the expected location carries the platform
    // separator, exactly like the discovered path's own display.
    assert_eq!(
        timings.regions[0].location,
        world_path
            .join("region")
            .join("r.00.00.mca")
            .to_string_lossy()
            .into_owned()
    );
}

/// The timing rows keep the world's own order.
///
/// The ingest runs in parallel, so the files complete in whatever order
/// the workers finish them; the report sorts by region identity, which
/// is the order every other operation reports. Sorting by the file name
/// would not: `r.10.0.mca` sorts before `r.2.0.mca` as a name, while
/// region 2 comes before region 10 as an identity.
#[tokio::test]
async fn timing_rows_are_ordered_by_region_identity() {
    let (root, _guard) = tempdir("timing-order");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    for (name, chunk) in [
        ("r.10.0.mca", (320, 0)),
        ("r.2.0.mca", (64, 0)),
        ("r.0.10.mca", (0, 320)),
        ("r.0.2.mca", (0, 64)),
    ] {
        write_region(
            world_path.join("region").join(name),
            &[(chunk.0, chunk.1, vec![3, 1])],
        );
    }

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    let (_, timings) = instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    let locations: Vec<&str> = timings
        .regions
        .iter()
        .map(|region| region.location.as_str())
        .collect();
    assert_eq!(locations.len(), 4);
    // Identity order: x before z, and the coordinates numerically.
    let expected = ["r.0.2.mca", "r.0.10.mca", "r.2.0.mca", "r.10.0.mca"];
    for (location, name) in locations.iter().zip(expected) {
        assert!(location.ends_with(name), "unexpected order: {locations:?}");
    }
}

#[tokio::test]
async fn scoped_commit_records_no_spurious_tombstones() {
    use sekai_app::{ChunkCoord, Dimension, Scope};
    let (root, _guard) = tempdir("scoped");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let over = world_path.join("region/r.0.0.mca");
    let nether = world_path.join("DIM-1/region/r.0.0.mca");
    // Minimal valid chunk payload so chunk diffs parse.
    let payload = build_status_nbt("minecraft:full");
    write_region(&over, &[(0, 0, payload.clone()), (1, 0, payload.clone())]);
    write_region(&nether, &[(0, 0, payload.clone())]);

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    let (full, _) = instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, |_| {})
        .await
        .unwrap();
    assert_eq!(full.chunks, 3);
    assert_eq!(full.tombstones, 0);

    // Overworld-only commit with no changes: nothing ingested, and the
    // out-of-scope nether chunk must not become a tombstone.
    let (scoped, _) = instance
        .world_mut(&mut world)
        .commit(options(), Scope::dimension(Dimension::OVERWORLD), |_| {})
        .await
        .unwrap();
    assert_eq!(scoped.tombstones, 0);
    assert_eq!(scoped.chunks, 2);

    // The nether chunk still resolves through fallback to the full commit.
    let commits = instance.list_commits().await.unwrap();
    assert_eq!(commits.len(), 2);
    let nether_coord = ChunkCoord::new(Dimension::NETHER, RegionKind::Region, 0, 0);
    let diffs = instance
        .diff_chunk(commits[0].id, commits[1].id, &nether_coord)
        .await
        .unwrap();
    assert_eq!(diffs, ChunkPayloadDiff::Nothing);

    // Vanish one in-scope chunk: exactly that tombstone is recorded.
    write_region(&over, &[(0, 0, payload)]);
    let (scoped2, _) = instance
        .world_mut(&mut world)
        .commit(options(), Scope::dimension(Dimension::OVERWORLD), |_| {})
        .await
        .unwrap();
    assert_eq!(scoped2.tombstones, 1);

    // A following full commit sees no further changes: zero tombstones,
    // zero new blobs, nether chunk intact. The carried overworld file must
    // not resurrect its tombstoned chunk in the present count either.
    let (full2, _) = instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, |_| {})
        .await
        .unwrap();
    assert_eq!(full2.tombstones, 0);
    assert_eq!(full2.new_blobs, 0);
    assert_eq!(full2.chunks, 2);
    let commits = instance.list_commits().await.unwrap();
    assert_eq!(commits.len(), 4);
    let diffs = instance
        .diff_chunk(commits[0].id, commits[3].id, &nether_coord)
        .await
        .unwrap();
    assert_eq!(diffs, ChunkPayloadDiff::Nothing);
}

/// A partial scope reads only part of a region; the next full commit must
/// reread the whole file and record the chunks the scoped commit skipped
/// (the #90 regression guard).
#[tokio::test]
async fn partial_scope_then_full_commit_reingests_unread_chunks() {
    use sekai_app::{Area, Dimension, Rect};
    let (root, _guard) = tempdir("partial-scope");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let region = world_path.join("region/r.0.0.mca");
    write_region(
        &region,
        &[(0, 0, build_status_nbt("a")), (1, 0, build_status_nbt("b"))],
    );

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, |_| {})
        .await
        .unwrap();

    // Both chunks change, but the scoped commit reads only (0,0).
    write_region(
        &region,
        &[(0, 0, build_status_nbt("c")), (1, 0, build_status_nbt("d"))],
    );
    let partial = Scope::Select {
        kinds: vec![RegionKind::Region],
        areas: vec![(Dimension::OVERWORLD, Area::Rect(Rect::new(0, 0, 0, 0)))],
    };
    let (scoped, _) = instance
        .world_mut(&mut world)
        .commit(options(), partial, |_| {})
        .await
        .unwrap();
    assert_eq!(scoped.chunks, 1);
    assert_eq!(scoped.new_blobs, 1);

    // The full commit rereads the file: only the previously unread chunk
    // needs a new blob, and its content reaches history.
    let (full, _) = instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, |_| {})
        .await
        .unwrap();
    assert_eq!(full.chunks, 2);
    assert_eq!(full.new_blobs, 1);
    assert_eq!(full.tombstones, 0);

    let commits = instance.list_commits().await.unwrap();
    assert_eq!(commits.len(), 3);
    let coord =
        sekai_core::ChunkCoord::new(sekai_core::Dimension::OVERWORLD, RegionKind::Region, 1, 0);
    let diffs = instance
        .diff_chunk(commits[1].id, commits[2].id, &coord)
        .await
        .unwrap();
    assert!(!diffs.is_empty());

    // A repeated full commit has nothing left to write.
    let (quiet, _) = instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, |_| {})
        .await
        .unwrap();
    assert_eq!(quiet.chunks, 2);
    assert_eq!(quiet.new_blobs, 0);
    assert_eq!(quiet.tombstones, 0);
}

/// A world that exists when the tree is opened is classified rather than
/// treated as the missing-world fallback, and its trio keeps the constants
/// scoped operations address by name.
#[tokio::test]
async fn scoped_commit_addresses_a_detected_trio_world() {
    use sekai_app::{Dimension, Scope};
    let (root, _guard) = tempdir("scoped-detected");
    let world_path = root.join("world");
    let store = root.join("store").to_string_lossy().into_owned();
    // 26.1 layout, written before the tree is opened so `new` classifies it
    // (an unrecognizable root falls back to the legacy tree).
    write_region(
        world_path.join("dimensions/minecraft/overworld/region/r.0.0.mca"),
        &[(0, 0, vec![3, 1]), (1, 0, vec![3, 2])],
    );
    write_region(
        world_path.join("dimensions/minecraft/the_nether/region/r.0.0.mca"),
        &[(0, 0, vec![3, 3])],
    );
    let mut world = HostWorldTree::new(&world_path).unwrap();

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    let (report, _) = instance
        .world_mut(&mut world)
        .commit(options(), Scope::dimension(Dimension::OVERWORLD), |_| {})
        .await
        .unwrap();
    // Only the overworld chunks: the scope must address the discovered
    // overworld, not silently match nothing (or the nether).
    assert_eq!(report.chunks, 2);
    assert_eq!(report.tombstones, 0);
}

#[tokio::test]
async fn scoped_checkout_leaves_other_dimensions_untouched() {
    use sekai_app::{Dimension, Scope};
    let (root, _guard) = tempdir("scoped-checkout");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let over = world_path.join("region/r.0.0.mca");
    let nether = world_path.join("DIM-1/region/r.0.0.mca");
    write_region(&over, &[(0, 0, vec![3, 1])]);
    write_region(&nether, &[(0, 0, vec![3, 2])]);

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, |_| {})
        .await
        .unwrap();
    let commits = instance.list_commits().await.unwrap();

    // Diverge both dimensions, then delete the nether file outright.
    write_region(&over, &[(0, 0, vec![3, 9])]);
    write_region(&nether, &[(0, 0, vec![3, 8])]);
    let nether_diverged = std::fs::read(&nether).unwrap();
    std::fs::remove_file(&nether).unwrap();

    // Overworld-scoped checkout: the overworld reverts, the nether file
    // is neither recreated nor deleted (it stays absent), and the report
    // counts only in-scope work.
    let (checked_out, _) = instance
        .world_mut(&mut world)
        .checkout(
            commits[0].id,
            sekai_app::CheckoutOptions::default(),
            Scope::dimension(Dimension::OVERWORLD),
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(checked_out.files_written, 1);
    assert_eq!(checked_out.files_deleted, 0);
    assert_eq!(checked_out.chunks_restored, 1);
    assert_eq!(read_coords(&over), vec![(0, 0)]);
    assert!(!nether.exists());

    // Nether-scoped checkout recreates the deleted file from CAS.
    let (checked_out, _) = instance
        .world_mut(&mut world)
        .checkout(
            commits[0].id,
            sekai_app::CheckoutOptions::default(),
            Scope::dimension(Dimension::NETHER),
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(checked_out.files_written, 1);
    assert_eq!(checked_out.chunks_restored, 1);
    assert_eq!(read_coords(&nether), vec![(0, 0)]);

    // Overworld-scoped checkout never rewrites an in-place nether file.
    write_region(&nether, &[(0, 0, vec![3, 8])]);
    let before = std::fs::read(&nether).unwrap();
    assert_eq!(before, nether_diverged);
    instance
        .world_mut(&mut world)
        .checkout(
            commits[0].id,
            sekai_app::CheckoutOptions::default(),
            Scope::dimension(Dimension::OVERWORLD),
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(std::fs::read(&nether).unwrap(), before);
}

#[tokio::test]
async fn strict_checkout_removes_post_commit_files() {
    let (root, _guard) = tempdir("strict");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let kept = world_path.join("region/r.0.0.mca");
    write_region(&kept, &[(0, 0, vec![3, 1])]);

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();

    // Afterwards: the known chunk vanishes and a new region appears.
    let added = world_path.join("region/r.1.0.mca");
    write_region(&added, &[(32, 0, vec![3, 2])]);
    write_region(&kept, &[]);
    instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    let commits = instance.list_commits().await.unwrap();
    assert_eq!(commits.len(), 2);

    // Commit 2: all-tombstone region deleted (not shelled), new chunk kept.
    let (checked_out, _) = instance
        .world_mut(&mut world)
        .checkout(
            commits[1].id,
            sekai_app::CheckoutOptions::default(),
            sekai_app::Scope::World,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(checked_out.files_written, 1);
    assert_eq!(checked_out.files_deleted, 1);
    assert_eq!(checked_out.chunks_restored, 1);
    assert!(!kept.exists());
    assert_eq!(read_coords(&added), vec![(32, 0)]);

    // Commit 1: original chunk rebuilt, post-commit file removed.
    let (checked_out, _) = instance
        .world_mut(&mut world)
        .checkout(
            commits[0].id,
            sekai_app::CheckoutOptions::default(),
            sekai_app::Scope::World,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(checked_out.files_written, 1);
    assert_eq!(checked_out.files_deleted, 1);
    assert_eq!(read_coords(&kept), vec![(0, 0)]);
    assert!(!added.exists());
}

#[tokio::test]
async fn keep_options_preserve_post_commit_data() {
    use sekai_app::CheckoutOptions;
    let (root, _guard) = tempdir("keep-post");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let known = world_path.join("region/r.0.0.mca");
    write_region(&known, &[(0, 0, vec![3, 1])]);

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    let commits = instance.list_commits().await.unwrap();

    // Afterwards: a new chunk inside the known region plus a new file.
    let added = world_path.join("region/r.1.0.mca");
    write_region(&known, &[(0, 0, vec![3, 1]), (1, 0, vec![3, 2])]);
    write_region(&added, &[(32, 0, vec![3, 3])]);

    let keep = CheckoutOptions {
        keep_post_commit_files: true,
        keep_post_commit_chunks: true,
        ..CheckoutOptions::default()
    };
    let (checked_out, _) = instance
        .world_mut(&mut world)
        .checkout(commits[0].id, keep, sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    assert_eq!(checked_out.files_written, 1);
    assert_eq!(checked_out.files_deleted, 0);
    assert_eq!(checked_out.chunks_restored, 1);
    let map = chunk_map(&known);
    assert_eq!(map.get(&(0, 0)), Some(&vec![3, 1]));
    assert_eq!(map.get(&(1, 0)), Some(&vec![3, 2]));
    assert_eq!(read_coords(&added), vec![(32, 0)]);
}

#[tokio::test]
async fn keep_tombstoned_chunks_preserves_live_bytes() {
    use sekai_app::CheckoutOptions;
    let (root, _guard) = tempdir("keep-tomb");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let region = world_path.join("region/r.0.0.mca");
    write_region(&region, &[(0, 0, vec![3, 1]), (1, 0, vec![3, 2])]);

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    // Delete one chunk: the second commit tombstones it.
    write_region(&region, &[(0, 0, vec![3, 1])]);
    instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    let commits = instance.list_commits().await.unwrap();
    assert_eq!(commits.len(), 2);

    // Diverge live: change the kept chunk, recreate the deleted one.
    write_region(&region, &[(0, 0, vec![3, 9]), (1, 0, vec![3, 8])]);

    // Strict checkout drops the tombstoned chunk and reverts the rest.
    let (checked_out, _) = instance
        .world_mut(&mut world)
        .checkout(
            commits[1].id,
            sekai_app::CheckoutOptions::default(),
            sekai_app::Scope::World,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(checked_out.chunks_restored, 1);
    assert_eq!(read_coords(&region), vec![(0, 0)]);
    assert_eq!(chunk_map(&region)[&(0, 0)], vec![3, 1]);

    // Keep checkout preserves the live bytes of the tombstoned chunk.
    write_region(&region, &[(0, 0, vec![3, 9]), (1, 0, vec![3, 8])]);
    let keep = CheckoutOptions {
        keep_tombstoned_chunks: true,
        ..CheckoutOptions::default()
    };
    let (checked_out, _) = instance
        .world_mut(&mut world)
        .checkout(commits[1].id, keep, sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    assert_eq!(checked_out.chunks_restored, 1);
    let map = chunk_map(&region);
    assert_eq!(map[&(0, 0)], vec![3, 1]);
    assert_eq!(map[&(1, 0)], vec![3, 8]);
}

#[tokio::test]
async fn keep_tombstoned_chunks_leaves_fully_tombstoned_files() {
    use sekai_app::CheckoutOptions;
    let (root, _guard) = tempdir("keep-tomb-file");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let region = world_path.join("region/r.0.0.mca");
    write_region(&region, &[(0, 0, vec![3, 1])]);

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    // Empty the region: the second commit tombstones its only chunk.
    write_region(&region, &[]);
    instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    let commits = instance.list_commits().await.unwrap();

    // Live recreates the chunk afterwards.
    write_region(&region, &[(0, 0, vec![3, 7])]);
    let keep = CheckoutOptions {
        keep_tombstoned_chunks: true,
        ..CheckoutOptions::default()
    };
    let (checked_out, _) = instance
        .world_mut(&mut world)
        .checkout(commits[1].id, keep, sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    assert_eq!(checked_out.files_written, 0);
    assert_eq!(checked_out.files_deleted, 0);
    assert_eq!(read_coords(&region), vec![(0, 0)]);

    // Strict checkout deletes the fully tombstoned file.
    let (checked_out, _) = instance
        .world_mut(&mut world)
        .checkout(
            commits[1].id,
            sekai_app::CheckoutOptions::default(),
            sekai_app::Scope::World,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(checked_out.files_deleted, 1);
    assert!(!region.exists());
}

#[tokio::test]
async fn missing_blob_abort_is_default_and_skip_recovers() {
    use sekai_app::{CheckoutOptions, MissingBlobPolicy};
    let (root, _guard) = tempdir("missing-blob");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store_dir = root.join("store");
    let store = store_dir.to_string_lossy().into_owned();
    let region = world_path.join("region/r.0.0.mca");
    write_region(&region, &[(0, 0, vec![3, 1])]);

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    let commits = instance.list_commits().await.unwrap();

    // Corrupt the CAS by unlinking every stored blob.
    let mut blobs = Vec::new();
    let mut dirs = vec![store_dir.join("blobs")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                blobs.push(path);
            }
        }
    }
    assert_ne!(blobs, [] as [std::path::PathBuf; 0]);
    for blob in &blobs {
        std::fs::remove_file(blob).unwrap();
    }

    // Default policy aborts loudly, leaving the live file alone.
    assert!(
        instance
            .world_mut(&mut world)
            .checkout(
                commits[0].id,
                sekai_app::CheckoutOptions::default(),
                sekai_app::Scope::World,
                |_| {},
            )
            .await
            .is_err()
    );
    assert_eq!(read_coords(&region), vec![(0, 0)]);

    // Skip policy rebuilds the file without the missing chunk.
    let skip = CheckoutOptions {
        on_missing_blob: MissingBlobPolicy::SkipChunk,
        ..CheckoutOptions::default()
    };
    let (checked_out, _) = instance
        .world_mut(&mut world)
        .checkout(commits[0].id, skip, sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    assert_eq!(checked_out.files_written, 1);
    assert_eq!(checked_out.chunks_restored, 0);
    assert_eq!(read_coords(&region), [] as [(i32, i32); 0]);
}

#[tokio::test]
async fn missing_file_error_policy_refuses_to_guess() {
    use sekai_app::{CheckoutOptions, MissingFilePolicy};
    let (root, _guard) = tempdir("missing-file");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let region = world_path.join("region/r.0.0.mca");
    write_region(&region, &[(0, 0, vec![3, 1])]);

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    let commits = instance.list_commits().await.unwrap();
    std::fs::remove_file(&region).unwrap();

    // Default policy recreates the file at the derived path.
    let (checked_out, _) = instance
        .world_mut(&mut world)
        .checkout(
            commits[0].id,
            sekai_app::CheckoutOptions::default(),
            sekai_app::Scope::World,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(checked_out.files_written, 1);
    assert_eq!(read_coords(&region), vec![(0, 0)]);

    // Error policy fails loudly instead.
    std::fs::remove_file(&region).unwrap();
    let strict = CheckoutOptions {
        on_missing_file: MissingFilePolicy::Error,
        ..CheckoutOptions::default()
    };
    assert!(
        instance
            .world_mut(&mut world)
            .checkout(commits[0].id, strict, sekai_app::Scope::World, |_| {})
            .await
            .is_err()
    );

    // Derived-only policy recreates without consulting siblings.
    let derived = CheckoutOptions {
        on_missing_file: MissingFilePolicy::DerivedOnly,
        ..CheckoutOptions::default()
    };
    let (checked_out, _) = instance
        .world_mut(&mut world)
        .checkout(commits[0].id, derived, sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    assert_eq!(checked_out.files_written, 1);
    assert_eq!(read_coords(&region), vec![(0, 0)]);
}

#[tokio::test]
async fn with_diff_records_diff_hashes() {
    use sekai_core::MetaStore as _;
    let (root, _guard) = tempdir("diff");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store_dir = root.join("store");
    let store_url = store_dir.to_string_lossy().into_owned();
    let payload = build_status_nbt("minecraft:full");
    write_region(world_path.join("region/r.0.0.mca"), &[(0, 0, payload)]);

    let options = CommitOptions {
        with_diff: true,
        ..options()
    };
    let mut instance = sekai_app::SekaiInstance::init(&store_url).await.unwrap();
    let (report, _) = instance
        .world_mut(&mut world)
        .commit(options, sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    assert_eq!(report.chunks, 1);

    let commits = instance.list_commits().await.unwrap();
    let meta = sekai_storage_sqlite::open_sqlite(&store_dir).await.unwrap();
    let row = meta
        .meta()
        .lookup_chunk(
            commits[0].id,
            &sekai_core::ChunkCoord::new(
                sekai_core::Dimension::OVERWORLD,
                sekai_core::RegionKind::Region,
                0,
                0,
            ),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(row.blob.is_some());
    assert!(row.diff.is_some());
}

#[tokio::test]
async fn progress_fires_per_changed_file() {
    use std::sync::{Arc, Mutex};
    let (root, _guard) = tempdir("progress");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    write_region(world_path.join("region/r.0.0.mca"), &[(0, 0, vec![3, 1])]);
    write_region(world_path.join("region/r.1.0.mca"), &[(32, 0, vec![3, 2])]);

    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&events);
    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, move |p| {
            seen.lock().unwrap().push((p.files_done, p.files_total));
        })
        .await
        .unwrap();
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|&(_, total)| total == 2));
    assert_eq!(events.last().unwrap().0, 2);
}

#[tokio::test]
async fn errors_surface_loudly() {
    let (root, _guard) = tempdir("errors");
    let store = root.join("store").to_string_lossy().into_owned();
    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    // Missing world.
    let mut missing = HostWorldTree::new(root.join("nope")).unwrap();
    assert!(
        instance
            .world_mut(&mut missing)
            .commit(options(), sekai_app::Scope::World, |_| {})
            .await
            .is_err()
    );
    // Unknown commit (empty store is created on open, then lookup fails).
    let mut world = HostWorldTree::new(root.join("world")).unwrap();
    assert!(
        instance
            .world_mut(&mut world)
            .checkout(
                CommitId([99; 32]),
                sekai_app::CheckoutOptions::default(),
                sekai_app::Scope::World,
                |_| {},
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn gc_runs_and_returns_timings() {
    let (root, _guard) = tempdir("gc");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    write_region(world_path.join("region/r.0.0.mca"), &[(0, 0, vec![3, 1])]);

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();

    let (plan, plan_timings) = instance.gc_plan().await.unwrap();
    assert_eq!(plan.orphans, [] as [sekai_app::BlobHash; 0]);
    assert_eq!(plan_timings.apply, std::time::Duration::ZERO);

    let (report, timings) = instance.gc(|_| {}).await.unwrap();
    assert_eq!(report.candidates, 0);
    assert_eq!(report.removed, 0);
    assert!(timings.total >= timings.plan + timings.apply);
}

#[tokio::test]
async fn progress_events_cover_checkout_diff_and_gc() {
    use sekai_app::{CheckoutProgress, DiffProgress, GcProgress};
    use std::sync::{Arc, Mutex};
    let (root, _guard) = tempdir("progress-events");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, build_status_nbt("minecraft:full"))],
    );
    write_region(
        world_path.join("region/r.1.0.mca"),
        &[(32, 0, build_status_nbt("minecraft:full"))],
    );

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    let commits = instance.list_commits().await.unwrap();

    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&events);
    instance
        .world_mut(&mut world)
        .checkout(
            commits[0].id,
            sekai_app::CheckoutOptions::default(),
            sekai_app::Scope::World,
            move |p: CheckoutProgress| {
                seen.lock().unwrap().push((p.files_done, p.files_total));
            },
        )
        .await
        .unwrap();
    {
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|&(_, total)| total == 2));
        assert_eq!(events.last().unwrap().0, 2);
    }

    let coord = sekai_app::ChunkCoord::new(
        sekai_app::Dimension::OVERWORLD,
        sekai_app::RegionKind::Region,
        0,
        0,
    );
    let seen_diff = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = Arc::clone(&seen_diff);
    let (diffs, _) = instance
        .diff_chunks(
            commits[0].id,
            commits[0].id,
            &[coord],
            move |p: DiffProgress| {
                seen_clone
                    .lock()
                    .unwrap()
                    .push((p.chunks_done, p.chunks_total));
            },
        )
        .await
        .unwrap();
    assert!(diffs.iter().all(|diff| diff.entries.is_empty()));
    assert_eq!(&*seen_diff.lock().unwrap(), &[(1, 1)]);

    let seen_gc = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = Arc::clone(&seen_gc);
    let (report, _) = instance
        .gc(move |p: GcProgress| {
            seen_clone
                .lock()
                .unwrap()
                .push((p.blobs_done, p.blobs_total));
        })
        .await
        .unwrap();
    assert_eq!(report.candidates, 0);
    assert!(seen_gc.lock().unwrap().is_empty());
}

#[tokio::test]
async fn checkout_progress_completes_for_tombstone_only_region() {
    use std::sync::{Arc, Mutex};
    let (root, _guard) = tempdir("progress-tombstone-only");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();

    let doomed = world_path.join("poi/r.0.0.mca");
    write_region(&doomed, &[(0, 0, vec![3, 1, 2, 3])]);
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, vec![3, 8, 8, 8])],
    );

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();

    instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();

    std::fs::remove_file(&doomed).unwrap();

    let (second, _) = instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();

    assert_eq!(second.tombstones, 1);

    let commit_id = instance.latest_commit_id().await.unwrap();

    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&events);

    let (report, _) = instance
        .world_mut(&mut world)
        .checkout(
            commit_id,
            sekai_app::CheckoutOptions::default(),
            sekai_app::Scope::World,
            move |p| {
                seen.lock().unwrap().push((p.files_done, p.files_total));
            },
        )
        .await
        .unwrap();

    assert_eq!(report.files_written, 1);
    assert_eq!(report.files_deleted, 0);
    assert!(!doomed.exists());

    let events = events.lock().unwrap();
    assert_eq!(*events, [(1, 2), (2, 2)]);
}

#[tokio::test]
async fn diff_chunk_between_commits() {
    let (root, _guard) = tempdir("diff_chunk");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let region = world_path.join("region/r.0.0.mca");

    write_region(&region, &[(0, 0, build_status_nbt("minecraft:full"))]);
    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();

    // Same millisecond-tick hazard as below: separate ticks so the
    // same-size rewrite is observed instead of carried.
    std::thread::sleep(std::time::Duration::from_millis(5));
    write_region(&region, &[(0, 0, build_status_nbt("minecraft:empty"))]);
    instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();

    let commits = instance.list_commits().await.unwrap();
    assert_eq!(commits.len(), 2);

    let coord = sekai_core::ChunkCoord::new(
        sekai_core::Dimension::OVERWORLD,
        sekai_core::RegionKind::Region,
        0,
        0,
    );

    let diffs = instance
        .diff_chunk(commits[0].id, commits[1].id, &coord)
        .await
        .unwrap();

    assert!(
        matches!(diffs, ChunkPayloadDiff::Full { old, new } if old != new),
        "status change must report a full diff with distinct hashes: {diffs:?}"
    );
}

#[tokio::test]
async fn diff_chunk_propagates_malformed_region_nbt() {
    let (root, _guard) = tempdir("diff-malformed");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let region = world_path.join("region/r.0.0.mca");
    let coord = sekai_core::ChunkCoord::new(
        sekai_core::Dimension::OVERWORLD,
        sekai_core::RegionKind::Region,
        0,
        0,
    );

    write_region(&region, &[(0, 0, build_status_nbt("minecraft:full"))]);
    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();

    // Valid NBT (an empty compound), but not a level-chunk payload.
    std::thread::sleep(std::time::Duration::from_millis(5));
    write_region(&region, &[(0, 0, vec![3, 0x0a, 0, 0, 0])]);
    instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();

    let commits = instance.list_commits().await.unwrap();
    let err = instance
        .diff_chunk(commits[0].id, commits[1].id, &coord)
        .await
        .unwrap_err();
    assert!(
        matches!(err, sekai_app::AppError::ChunkNbt { .. }),
        "malformed region payload must fail loudly: {err:?}"
    );
}

#[tokio::test]
async fn diff_world_chunk_with_commit() {
    let (root, _guard) = tempdir("diff_world_chunk");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let region = world_path.join("region/r.0.0.mca");

    write_region(&region, &[(0, 0, build_status_nbt("minecraft:full"))]);
    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();

    write_region(&region, &[(0, 0, build_status_nbt("minecraft:empty"))]);

    let coord = sekai_core::ChunkCoord::new(
        sekai_core::Dimension::OVERWORLD,
        sekai_core::RegionKind::Region,
        0,
        0,
    );

    let diffs = instance
        .world(&world)
        .diff_world_chunk(None, &coord)
        .await
        .unwrap();

    assert!(
        matches!(diffs, ChunkPayloadDiff::Full { old, new } if old != new),
        "status change must report a full diff with distinct hashes: {diffs:?}"
    );
}

/// Diff must read the discovered `r.00.00.mca`, not the canonical
/// `r.0.0.mca` that does not exist on disk.
#[tokio::test]
async fn diffs_a_non_canonical_region_name() {
    let (root, _guard) = tempdir("diff-non-canonical");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let region = world_path.join("region/r.00.00.mca");

    write_region(&region, &[(0, 0, build_status_nbt("minecraft:full"))]);
    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();

    write_region(&region, &[(0, 0, vec![3, 4, 5, 6])]);

    let coord = sekai_app::ChunkCoord::new(
        sekai_app::Dimension::OVERWORLD,
        sekai_app::RegionKind::Region,
        0,
        0,
    );
    // The commit side holds valid NBT, so the decode failure can only
    // come from the world side: it proves the discovered file was read.
    let err = instance
        .world(&world)
        .diff_world_chunk(None, &coord)
        .await
        .expect_err("raw payload is not valid NBT");
    assert!(
        matches!(err, sekai_app::AppError::ChunkNbt { .. }),
        "diff must read the discovered file, got: {err}"
    );
}

#[tokio::test]
async fn commits_region_with_trailing_partial_sector() {
    // Real files can carry a torn tail (e.g. 1034 sectors + 188 bytes)
    // that vanilla opens fine; commit must accept it as long as every
    // referenced run is intact.
    let (root, _guard) = tempdir("partial-tail");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let region = world_path.join("region/r.0.0.mca");
    write_region(&region, &[(0, 0, vec![3, 1])]);

    let mut bytes = std::fs::read(&region).unwrap();
    bytes.extend_from_slice(&[0xAB; 188]);
    assert_ne!(bytes.len() % 4096, 0);
    std::fs::write(&region, &bytes).unwrap();

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    let (report, _) = instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    assert_eq!(report.chunks, 1);
    assert_eq!(report.new_blobs, 1);
    assert_eq!(read_coords(&region), vec![(0, 0)]);
}

#[tokio::test]
async fn corrupt_region_names_its_file() {
    // Genuine corruption still fails loudly, now naming the file.
    let (root, _guard) = tempdir("corrupt-names-file");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let region = world_path.join("region/r.0.0.mca");
    write_region(&region, &[(0, 0, vec![3, 1])]);

    // Point the only entry far past end of file.
    let mut bytes = std::fs::read(&region).unwrap();
    bytes[0..4].copy_from_slice(&((9u32 << 8 | 1).to_be_bytes()));
    std::fs::write(&region, &bytes).unwrap();

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    let err = instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap_err();
    let message = format!("{err}");
    assert!(
        message.contains("r.0.0.mca"),
        "error names the file: {message}"
    );
}

fn build_status_nbt(status: &str) -> Vec<u8> {
    let chunk = sekai_nbt::nanonbt::to_bytes(&sekai_nbt::ChunkPayload {
        data_version: 1,
        x_pos: 0,
        y_pos: 0,
        z_pos: 0,
        inhabited_time: 0,
        last_update: 0,
        sections: vec![],
        heightmaps: sekai_nbt::Heightmaps::default(),
        status: &Cesu8::from_str(status),
        block_entities: vec![],
        block_ticks: vec![],
        fluid_ticks: vec![],
    })
    .unwrap();
    let mut payload = vec![3]; // Uncompressed header byte
    payload.extend(chunk);
    payload
}

const fn kind_dir(kind: sekai_app::RegionKind) -> &'static str {
    match kind {
        sekai_app::RegionKind::Region => "region",
        sekai_app::RegionKind::Entities => "entities",
        sekai_app::RegionKind::Poi => "poi",
    }
}

#[tokio::test]
async fn sparse_entities_chunk_diffs_empty_without_error() {
    use sekai_app::{ChunkCoord, Dimension};
    let (root, _guard) = tempdir("sparse-entities");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    // Dense terrain only; no entities/poi files at all.
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, build_status_nbt("minecraft:full"))],
    );

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    let commits = instance.list_commits().await.unwrap();

    // The sparse kinds have no (0,0): previously a loud error, now empty.
    for kind in [RegionKind::Entities, RegionKind::Poi] {
        let coord = ChunkCoord::new(Dimension::OVERWORLD, kind, 0, 0);
        let diffs = instance
            .diff_chunk(commits[0].id, commits[0].id, &coord)
            .await
            .unwrap();
        assert!(diffs.is_empty());
        let world_diffs = instance
            .world(&world)
            .diff_world_chunk(None, &coord)
            .await
            .unwrap();
        assert!(world_diffs.is_empty());
    }
}

/// A level chunk whose NBT lacks the loaded-chunk schema (legacy format,
/// `xPos` absent) fails the diff with the chunk and region file named, so
/// the offending part of the world can be located.
#[tokio::test]
async fn malformed_level_chunk_diff_names_its_coord() {
    use sekai_app::{ChunkCoord, Dimension};
    let (root, _guard) = tempdir("malformed-level");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    // Chunk (33, 1) lives in r.1.0.mca: the region must come from the
    // coord, not from the file name of whatever failed to parse.
    write_region(
        world_path.join("region/r.1.0.mca"),
        &[(33, 1, build_status_nbt("minecraft:full"))],
    );
    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();

    // Rewrite the same cell with a payload missing the level-chunk fields.
    let mut payload = vec![3];
    payload.extend(legacy_chunk_nbt());
    write_region(world_path.join("region/r.1.0.mca"), &[(33, 1, payload)]);

    let coord = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Region, 33, 1);
    let err = instance
        .world(&world)
        .diff_world_chunk(None, &coord)
        .await
        .unwrap_err();
    let message = format!("{err}");
    assert!(
        message.contains("region 33,1") && message.contains("r.1.0.mca"),
        "error names the chunk and region file: {message}"
    );
    assert!(message.contains("xPos"), "keeps the NBT cause: {message}");
}

/// NBT of a pre-1.13 chunk: `xPos` is absent, so it cannot parse as a
/// [`sekai_nbt::ChunkPayload`].
fn legacy_chunk_nbt() -> Vec<u8> {
    const FIELDS: [(&[u8], i32); 2] = [(b"DataVersion", 0), (b"zPos", 0)];
    let mut out = vec![10, 0, 0]; // Compound root, empty name
    for (name, value) in FIELDS {
        let len = u16::try_from(name.len()).unwrap();
        out.push(3); // TAG_Int
        out.extend(len.to_be_bytes());
        out.extend(name);
        out.extend(value.to_be_bytes());
    }
    out.push(0); // TAG_End
    out
}

#[tokio::test]
async fn created_and_deleted_chunks_diff_as_added_removed() {
    use sekai_app::{ChunkCoord, Dimension};
    let (root, _guard) = tempdir("created-deleted");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let entities = world_path.join("entities/r.0.0.mca");
    let coord = ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Entities, 0, 0);

    // S1: no entities chunk.
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, build_status_nbt("minecraft:full"))],
    );
    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();

    // S2: entities chunk appears.
    write_region(&entities, &[(0, 0, build_status_nbt("minecraft:full"))]);
    instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    let commits = instance.list_commits().await.unwrap();

    let diffs = instance
        .diff_chunk(commits[0].id, commits[1].id, &coord)
        .await
        .unwrap();
    assert!(!diffs.is_empty());

    // S3: entities chunk vanishes again.
    std::fs::remove_file(&entities).unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    let commits = instance.list_commits().await.unwrap();
    let diffs = instance
        .diff_chunk(commits[1].id, commits[2].id, &coord)
        .await
        .unwrap();
    assert!(!diffs.is_empty());
}

#[tokio::test]
async fn all_kinds_round_trip() {
    use sekai_app::{ChunkCoord, Dimension};
    for kind in RegionKind::ALL {
        let (root, _guard) = tempdir(&format!("kind-{}", kind_dir(kind)));
        let world_path = root.join("world");
        let mut world = HostWorldTree::new(&world_path).unwrap();
        let store = root.join("store").to_string_lossy().into_owned();
        let file = world_path.join(format!("{}/r.0.0.mca", kind_dir(kind)));
        let coord = ChunkCoord::new(Dimension::OVERWORLD, kind, 0, 0);

        write_region(&file, &[(0, 0, build_status_nbt("minecraft:full"))]);
        let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
        instance
            .world_mut(&mut world)
            .commit(options(), sekai_app::Scope::World, |_| {})
            .await
            .unwrap();

        // Fingerprints compare mtime at millisecond resolution; tiny worlds
        // back up within one tick, and same-size/same-header files would
        // wrongly carry. Separate the ticks so the rewrite is observed.
        std::thread::sleep(std::time::Duration::from_millis(5));
        write_region(&file, &[(0, 0, build_status_nbt("minecraft:empty"))]);
        instance
            .world_mut(&mut world)
            .commit(options(), sekai_app::Scope::World, |_| {})
            .await
            .unwrap();
        let commits = instance.list_commits().await.unwrap();

        let diffs = instance
            .diff_chunk(commits[0].id, commits[1].id, &coord)
            .await
            .unwrap();
        assert!(!diffs.is_empty(), "kind {}", kind_dir(kind));

        let (checked_out, _) = instance
            .world_mut(&mut world)
            .checkout(
                commits[0].id,
                sekai_app::CheckoutOptions::default(),
                sekai_app::Scope::World,
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(checked_out.chunks_restored, 1);
    }
}

#[tokio::test]
async fn rect_and_kind_scopes_limit_ingest() {
    use sekai_app::{Area, Dimension, Rect, Scope};
    let (root, _guard) = tempdir("rect-kind");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, build_status_nbt("a")), (1, 0, build_status_nbt("b"))],
    );
    write_region(
        world_path.join("region/r.1.0.mca"),
        &[(32, 0, build_status_nbt("c"))],
    );
    write_region(
        world_path.join("entities/r.0.0.mca"),
        &[(0, 0, build_status_nbt("d"))],
    );

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, |_| {})
        .await
        .unwrap();

    // Rectangle over (0,0)-(1,0), region kind only: (32,0) and entities
    // stay out with no tombstones.
    let rect = Scope::Select {
        kinds: vec![RegionKind::Region],
        areas: vec![(Dimension::OVERWORLD, Area::Rect(Rect::new(0, 0, 1, 0)))],
    };
    let (report, _) = instance
        .world_mut(&mut world)
        .commit(options(), rect, |_| {})
        .await
        .unwrap();
    assert_eq!(report.chunks, 2);
    assert_eq!(report.tombstones, 0);

    // Kind filter alone: entities across the whole overworld.
    let entities = Scope::Select {
        kinds: vec![RegionKind::Entities],
        areas: vec![(Dimension::OVERWORLD, Area::All)],
    };
    let (report, _) = instance
        .world_mut(&mut world)
        .commit(options(), entities, |_| {})
        .await
        .unwrap();
    assert_eq!(report.chunks, 1);
    assert_eq!(report.tombstones, 0);

    // A following full commit is quiet: nothing spurious was recorded.
    let (full, _) = instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, |_| {})
        .await
        .unwrap();
    assert_eq!(full.tombstones, 0);
    assert_eq!(full.new_blobs, 0);
}

fn corpus_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("test-world")
}

fn copy_dir(src: impl AsRef<Path>, dst: impl AsRef<Path>) {
    std::fs::create_dir_all(dst.as_ref()).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.as_ref().join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

#[tokio::test]
async fn real_world_corpus_round_trip() {
    const REAL_WORLD_CHUNKS_COUNT: usize = 1024 + 151 + 14;

    let (root, _guard) = tempdir("corpus");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    copy_dir(corpus_path(), &world_path);
    let store = root.join("store").to_string_lossy().into_owned();

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    let (report, _) = instance
        .world_mut(&mut world)
        .commit(options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    assert_eq!(report.chunks, 1024 + 151 + 14);
    assert_eq!(report.tombstones, 0);

    // Real NBT in every kind decodes: self-diffs are empty, not errors.
    let commits = instance.list_commits().await.unwrap();
    for kind in RegionKind::ALL {
        let coords = sekai_app::world_chunk_coords(&world)
            .unwrap()
            .into_iter()
            .filter(|coord| coord.kind == kind)
            .collect::<Vec<_>>();
        assert!(!coords.is_empty(), "kind {kind:?} has chunks");
        let (diffs, _) = instance
            .diff_chunks(commits[0].id, commits[0].id, &coords, |_| {})
            .await
            .unwrap();
        assert!(diffs.iter().all(|diff| diff.entries.is_empty()));
    }

    // Checkout restores chunk contents (rebuilt files differ in layout,
    // so compare payloads, not raw bytes).
    let before: Vec<(PathBuf, ChunkMap)> = ["region", "entities", "poi"]
        .iter()
        .flat_map(|kind| std::fs::read_dir(world_path.join(kind)).unwrap())
        .map(|entry| {
            let path = entry.unwrap().path();
            let chunks = chunk_map(&path);
            (path, chunks)
        })
        .collect();
    let (checked_out, _) = instance
        .world_mut(&mut world)
        .checkout(
            commits[0].id,
            sekai_app::CheckoutOptions::default(),
            sekai_app::Scope::World,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(checked_out.chunks_restored, REAL_WORLD_CHUNKS_COUNT);
    for (path, chunks) in &before {
        assert_eq!(&chunk_map(path), chunks, "{path:?} restored");
    }
}

/// Model-based round trip over pseudo-random mutation sequences: commit
/// after every mutation, then check out each commit in turn and require
/// the world to match the recorded state byte-for-byte (per chunk payload).
///
/// This is the safety net for the delta-storage invariants that hand-written
/// scenarios miss: the effective state of a commit must equal the world it
/// captured, tombstones must resolve through fallback, and a restore must
/// reproduce files, chunks, and payloads exactly - across deleted files,
/// emptied regions, and same-size rewrites.
#[tokio::test(flavor = "multi_thread")]
async fn randomized_commit_checkout_round_trip() {
    const FILES: [(&str, i32, i32); 4] = [
        ("region/r.0.0.mca", 0, 0),
        ("region/r.1.0.mca", 1, 0),
        ("poi/r.0.0.mca", 0, 0),
        ("entities/r.1.1.mca", 1, 1),
    ];
    const SEEDS: u64 = 12;
    const STEPS: usize = 8;

    for seed in 1..=SEEDS {
        let (root, _guard) = tempdir(&format!("model{seed}"));
        let world_path = root.join("world");
        let mut world = HostWorldTree::new(&world_path).unwrap();
        std::fs::create_dir_all(&world_path).unwrap();
        let store = root.join("store").to_string_lossy().into_owned();
        let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
        let mut rng = Rng(seed | 1);
        let mut model: ChunkMaps = ChunkMaps::new();
        let mut history: Vec<ChunkMaps> = Vec::new();
        let mut id = 0u32;

        for step in 0..STEPS {
            let action = rng.below(100);
            let (name, rx, rz) = FILES[usize::try_from(rng.below(FILES.len() as u64)).unwrap()];
            let coord = (
                rx * 32 + i32::try_from(rng.below(6)).unwrap(),
                rz * 32 + i32::try_from(rng.below(6)).unwrap(),
            );
            match action {
                0..=29 => {
                    // (Re)create the file with one fresh chunk.
                    id += 1;
                    let mut chunks = BTreeMap::new();
                    chunks.insert(coord, model_payload(&format!("v{id}")));
                    model.insert(name.to_owned(), chunks);
                }
                30..=59 => {
                    // Same-chunk-count rewrite: replace one payload in place.
                    if let Some(chunks) = model.get_mut(name) {
                        let keys: Vec<(i32, i32)> = chunks.keys().copied().collect();
                        if !keys.is_empty() {
                            id += 1;
                            let pick = keys[usize::try_from(rng.below(keys.len() as u64)).unwrap()];
                            chunks.insert(pick, model_payload(&format!("{}", id % 100)));
                        }
                    }
                }
                60..=79 => {
                    if let Some(chunks) = model.get_mut(name) {
                        let keys: Vec<(i32, i32)> = chunks.keys().copied().collect();
                        if !keys.is_empty() {
                            let pick = keys[usize::try_from(rng.below(keys.len() as u64)).unwrap()];
                            chunks.remove(&pick);
                        }
                    }
                }
                80..=89 => {
                    model.remove(name);
                }
                _ => {}
            }
            // Only rewrite files whose bytes actually change, so unchanged
            // regions exercise the no-fresh-row path.
            materialize(&world_path, &model, &FILES);

            std::thread::sleep(std::time::Duration::from_millis(5));
            let (report, _) = instance
                .world_mut(&mut world)
                .commit(options(), Scope::World, |_| {})
                .await
                .unwrap_or_else(|e| panic!("seed {seed} step {step}: commit failed: {e}"));
            let expected: usize = model.values().map(BTreeMap::len).sum();
            assert_eq!(report.chunks, expected, "seed {seed} step {step}");

            // The commit's effective state is the world, tombstones
            // included; a fresh tombstone is recorded exactly once.
            let stats = instance.commit_stats(report.commit).await.unwrap();
            assert_eq!(
                stats.effective_chunks, expected,
                "seed {seed} step {step}: effective state"
            );
            assert_eq!(stats.fresh_tombstones, report.tombstones);
            history.push(model.clone());
        }

        // Strict checkout deletes fully tombstoned regions, so a header-only
        // file on disk is expected to be gone after a restore.
        let commits = instance.list_commits().await.unwrap();
        assert_eq!(commits.len(), history.len());
        for (index, commit) in commits.iter().enumerate().rev() {
            instance
                .world_mut(&mut world)
                .checkout(
                    commit.id,
                    sekai_app::CheckoutOptions::default(),
                    Scope::World,
                    |_| {},
                )
                .await
                .unwrap_or_else(|e| panic!("seed {seed}: checkout failed: {e}"));
            let want: ChunkMaps = history[index]
                .iter()
                .filter(|(_, chunks)| !chunks.is_empty())
                .map(|(name, chunks)| (name.clone(), chunks.clone()))
                .collect();
            assert_eq!(
                read_world(&world, &world_path),
                want,
                "seed {seed} commit {}",
                commit.id
            );
        }
    }
}

/// World model: root-relative region path -> chunk payloads.
type ChunkMaps = BTreeMap<String, BTreeMap<(i32, i32), Vec<u8>>>;

/// Uncompressed sector framing around a minimal NBT compound.
fn model_payload(id: &str) -> Vec<u8> {
    let mut nbt = vec![10u8, 0, 0, 8, 0, 2];
    nbt.extend_from_slice(b"id");
    nbt.extend_from_slice(&u16::try_from(id.len()).unwrap().to_be_bytes());
    nbt.extend_from_slice(id.as_bytes());
    nbt.push(0);
    let mut out = vec![3u8];
    out.extend_from_slice(&nbt);
    out
}

fn materialize(world_root: impl AsRef<Path>, model: &ChunkMaps, files: &[(&str, i32, i32)]) {
    for (name, _, _) in files {
        let path = world_root.as_ref().join(name);
        match model.get(*name) {
            Some(chunks) => {
                let image = {
                    let file_name = path.file_name().unwrap().to_str().unwrap();
                    let (rx, rz) = sekai_anvil::parse_region_name(file_name).unwrap();
                    let mut builder = sekai_anvil::RegionBuilder::new(rx, rz, 0).unwrap();
                    for ((x, z), bytes) in chunks {
                        builder.stage_chunk(*x, *z, bytes).unwrap();
                    }
                    builder.image().unwrap()
                };
                if std::fs::read(&path).ok().as_deref() != Some(image.as_slice()) {
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    std::fs::write(&path, image).unwrap();
                }
            }
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

fn read_world(world: &HostWorldTree, world_path: impl AsRef<Path>) -> ChunkMaps {
    let mut files = Vec::new();
    for dim in world.get_dims().unwrap() {
        for kind in RegionKind::ALL {
            for (_, region) in world.get_regions(dim, kind).unwrap() {
                files.push(region.as_path().to_path_buf());
            }
        }
    }
    files
        .into_iter()
        .map(|path| {
            // `/` regardless of platform, so the keys match the model's.
            let rel = path
                .strip_prefix(world_path.as_ref())
                .unwrap_or(&path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            (rel, chunk_map(&path))
        })
        .collect()
}

/// xorshift64: small, seeded, and dependency-free.
struct Rng(u64);

impl Rng {
    const fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    const fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}
