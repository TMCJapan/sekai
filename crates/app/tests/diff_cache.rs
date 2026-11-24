//! World-vs-commit diffs answered through the region-image cache
//! (ADR-0029, ADR-0033, ADR-0034).
//!
//! A proven-unchanged region skips per-chunk decompression entirely; a
//! cache miss, a byte mismatch, or a changed file falls back to the full
//! per-chunk path with identical results.

mod fixtures;

use fixtures::chunk_nbt;

use sekai_app::{
    BuildProgress, ChunkCoord, CommitOptions, Dimension, Progress, RegionKind, Scope, SekaiInstance,
};
use sekai_storage_local::LocalStore;
use sekai_world_local::HostWorldTree;
use std::path::Path;
use std::sync::Mutex;

/// A temp dir whose name starts with the calling test's name (tempfile
/// appends a random suffix), removed when its guard is dropped at the end
/// of the test, panics included.
fn tempdir(name: &str) -> (std::path::PathBuf, tempfile::TempDir) {
    let guard = tempfile::Builder::new()
        .prefix(&format!("sekai-app-diff-cache-{name}-"))
        .tempdir()
        .unwrap();
    (guard.path().to_path_buf(), guard)
}

/// Write a region image through the real builder, deriving the region
/// coordinates from the file name.
fn write_region(path: impl AsRef<Path>, chunks: &[(i32, i32, Vec<u8>)]) {
    write_region_at(path, chunks, 0);
}

/// Write a region image with an explicit header timestamp: the raw bytes
/// change while the normalized chunk hashes stay put.
fn write_region_at(path: impl AsRef<Path>, chunks: &[(i32, i32, Vec<u8>)], timestamp: u32) {
    let name = path.as_ref().file_name().unwrap().to_str().unwrap();
    let (rx, rz) = sekai_anvil::parse_region_name(name).unwrap();
    let mut builder = sekai_anvil::RegionBuilder::new(rx, rz, timestamp).unwrap();
    for (x, z, payload) in chunks {
        builder.stage_chunk(*x, *z, payload).unwrap();
    }
    let image = builder.image().unwrap();
    std::fs::create_dir_all(path.as_ref().parent().unwrap()).unwrap();
    std::fs::write(path, image).unwrap();
}

const fn options() -> CommitOptions {
    CommitOptions {}
}

const fn coord(x: i32, z: i32) -> ChunkCoord {
    ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Region, x, z)
}

/// Records every step of the one run it reports as `(done, total)`.
#[derive(Default)]
struct Recorded(Mutex<Recording>);

#[derive(Default)]
struct Recording {
    total: usize,
    events: Vec<(usize, usize)>,
}

impl Recorded {
    fn events(&self) -> Vec<(usize, usize)> {
        self.0.lock().unwrap().events.clone()
    }
}

impl BuildProgress for Recorded {
    fn build_progress(&self, total: usize) -> impl Progress {
        self.0.lock().unwrap().total = total;
        RecordedRun(&self.0)
    }
}

struct RecordedRun<'a>(&'a Mutex<Recording>);

impl Progress for RecordedRun<'_> {
    fn step(&mut self) {
        let mut recording = self.0.lock().unwrap();
        let event = (recording.events.len() + 1, recording.total);
        recording.events.push(event);
    }
}

/// An unchanged world diffs clean without decompressing a single chunk:
/// both sides answer from the committed region node.
#[tokio::test]
async fn unchanged_world_diff_skips_chunk_reads() {
    let (root, _guard) = tempdir("clean");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, chunk_nbt("marker-1")), (1, 0, chunk_nbt("marker-2"))],
    );

    let mut instance = SekaiInstance::<LocalStore>::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .update_cache(sekai_app::CacheUpdateOptions {}, &())
        .await
        .unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();

    let coords = [coord(0, 0), coord(1, 0)];
    let (diffs, timings) = instance
        .world(&world)
        .diff_world_chunks(None, &coords, &())
        .await
        .unwrap();
    assert!(diffs.iter().all(|diff| diff.entries.is_empty()));
    assert!(
        timings.decompress.is_zero(),
        "a proven region must not decompress chunks: {:?}",
        timings.decompress
    );
}

/// The cache is derived state: wiping it costs performance only. The diff
/// stays clean but falls back to per-chunk reads.
#[tokio::test]
async fn wiped_cache_falls_back_to_chunk_reads() {
    let (root, _guard) = tempdir("wiped");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store_dir = root.join("store");
    let store = store_dir.to_string_lossy().into_owned();
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, chunk_nbt("marker-1"))],
    );

    let mut instance = SekaiInstance::<LocalStore>::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .update_cache(sekai_app::CacheUpdateOptions {}, &())
        .await
        .unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();
    drop(instance);
    std::fs::remove_dir_all(store_dir.join("cache")).unwrap();
    let instance = SekaiInstance::<LocalStore>::init(&store).await.unwrap();

    let (diffs, timings) = instance
        .world(&world)
        .diff_world_chunks(None, &[coord(0, 0)], &())
        .await
        .unwrap();
    assert!(diffs.iter().all(|diff| diff.entries.is_empty()));
    assert!(
        !timings.decompress.is_zero(),
        "a cold cache must fall back to chunk reads"
    );
}

/// A changed chunk fails the whole-file proof: the region falls back to
/// per-chunk reads and the change is reported exactly as without a cache.
#[tokio::test]
async fn changed_chunk_falls_back_and_reports_the_change() {
    let (root, _guard) = tempdir("changed");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let path = world_path.join("region/r.0.0.mca");
    write_region(
        &path,
        &[(0, 0, chunk_nbt("marker-1")), (1, 0, chunk_nbt("marker-2"))],
    );

    let mut instance = SekaiInstance::<LocalStore>::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .update_cache(sekai_app::CacheUpdateOptions {}, &())
        .await
        .unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();
    write_region(
        &path,
        &[(0, 0, chunk_nbt("marker-9")), (1, 0, chunk_nbt("marker-2"))],
    );

    let coords = [coord(0, 0), coord(1, 0)];
    let (diffs, timings) = instance
        .world(&world)
        .diff_world_chunks(None, &coords, &())
        .await
        .unwrap();
    assert!(
        !diffs[0].entries.is_empty(),
        "the changed chunk must differ"
    );
    assert!(
        diffs[1].entries.is_empty(),
        "the kept chunk must not differ"
    );
    assert!(!timings.decompress.is_zero());
}

/// A header-only rewrite changes the raw bytes but no normalized chunk:
/// the proof fails on the byte mismatch, and the fallback still diffs
/// clean.
#[tokio::test]
async fn header_rewrite_falls_back_but_stays_clean() {
    let (root, _guard) = tempdir("header");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let path = world_path.join("region/r.0.0.mca");
    let chunks = [(0, 0, chunk_nbt("marker-1"))];
    write_region(&path, &chunks);

    let mut instance = SekaiInstance::<LocalStore>::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .update_cache(sekai_app::CacheUpdateOptions {}, &())
        .await
        .unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();
    write_region_at(&path, &chunks, 7);

    let (diffs, timings) = instance
        .world(&world)
        .diff_world_chunks(None, &[coord(0, 0)], &())
        .await
        .unwrap();
    assert!(diffs.iter().all(|diff| diff.entries.is_empty()));
    assert!(!timings.decompress.is_zero());
}

/// A deleted file stays unproven: the world side reports absence while
/// the commit side keeps its recorded digest.
#[tokio::test]
async fn deleted_file_reports_absence() {
    let (root, _guard) = tempdir("deleted");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let path = world_path.join("region/r.0.0.mca");
    write_region(&path, &[(0, 0, chunk_nbt("marker-1"))]);

    let mut instance = SekaiInstance::<LocalStore>::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .update_cache(sekai_app::CacheUpdateOptions {}, &())
        .await
        .unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();
    std::fs::remove_file(&path).unwrap();

    let (diffs, _) = instance
        .world(&world)
        .diff_world_chunks(None, &[coord(0, 0)], &())
        .await
        .unwrap();
    assert_eq!(diffs.len(), 1);
    let entries = &diffs[0].entries;
    assert!(entries.old.is_some(), "the commit still holds the chunk");
    assert!(entries.new.is_none(), "the world no longer holds the chunk");
}

/// Only the unchanged region is proven; the changed one falls back. Diff
/// progress still steps once per region file, in completion order.
#[tokio::test]
async fn mixed_regions_prove_only_the_unchanged_one() {
    let (root, _guard) = tempdir("mixed");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, chunk_nbt("marker-1")), (1, 0, chunk_nbt("marker-2"))],
    );
    let changed_path = world_path.join("region/r.1.0.mca");
    write_region(&changed_path, &[(32, 0, chunk_nbt("marker-3"))]);

    let mut instance = SekaiInstance::<LocalStore>::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .update_cache(sekai_app::CacheUpdateOptions {}, &())
        .await
        .unwrap();
    instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();
    write_region(&changed_path, &[(32, 0, chunk_nbt("marker-9"))]);

    // Deliberately unsorted, spanning both files.
    let coords = [coord(32, 0), coord(0, 0), coord(1, 0)];
    let recorded = Recorded::default();
    let (diffs, timings) = instance
        .world(&world)
        .diff_world_chunks(None, &coords, &recorded)
        .await
        .unwrap();
    assert!(
        !diffs[0].entries.is_empty(),
        "the changed region must differ"
    );
    assert!(diffs[1].entries.is_empty());
    assert!(diffs[2].entries.is_empty());
    assert!(!timings.decompress.is_zero());
    assert_eq!(recorded.events(), [(1, 2), (2, 2)]);
}
