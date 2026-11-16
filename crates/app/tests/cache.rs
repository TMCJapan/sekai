//! Region-image cache population through the commit-free update pass
//! (ADR-0029, ADR-0033, ADR-0034).
//!
//! `update_cache` keys every file under the region node its own chunks
//! produce; commit records history only and writes no cache entries.
//! `sekai commit` composes the two passes, so the flows here warm the
//! cache with `update_cache` before committing. Partial scopes, previews,
//! and empty files cache nothing, and wiping the cache costs performance
//! only.
//!
//! Requires the `backend-local` feature.

#![cfg(feature = "backend-local")]

mod fixtures;

use fixtures::chunk_nbt;

use sekai_app::{
    Area, CacheUpdateOptions, CommitId, CommitOptions, Dimension, HostWorldTree, Rect, RegionHash,
    RegionKey, RegionKind, Scope, SekaiStore, StatusOptions,
};
use sekai_core::hash_bytes;
use sekai_storage::{CacheStore as _, MetaStore as _, RegionEntry, RegionNode};
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
    CommitOptions { concurrency: 2 }
}

const fn update_options() -> CacheUpdateOptions {
    CacheUpdateOptions { concurrency: 2 }
}

/// The region node hash the commit names for `key`, if any.
async fn committed_region_hash<S: SekaiStore>(
    store: &S,
    commit: CommitId,
    key: RegionKey,
) -> Option<RegionHash> {
    let meta = store.meta();
    let record = meta.lookup_commit(commit).await.unwrap()?;
    let root = meta.read_root_node(record.root).await.unwrap();
    let dim_hash = root.get(key.dim.raw())?;
    let dims = meta.read_dim_nodes(&[dim_hash]).await.unwrap();
    dims[0].get(key.kind, key.rx, key.rz)
}

/// The region node hash the file's own chunks produce, computed straight
/// from the image: the cache key a full commit of this file must name.
fn file_region_hash(path: impl AsRef<Path>) -> RegionHash {
    let name = path.as_ref().file_name().unwrap().to_str().unwrap();
    let (rx, rz) = sekai_anvil::parse_region_name(name).unwrap();
    let bytes = std::fs::read(path).unwrap();
    let image = sekai_anvil::RegionImage::from_bytes(bytes, rx, rz).unwrap();
    let mut entries = Vec::new();
    image
        .visit_chunks(|chunk| {
            entries.push(RegionEntry {
                cx: u8::try_from(chunk.x.rem_euclid(32)).unwrap(),
                cz: u8::try_from(chunk.z.rem_euclid(32)).unwrap(),
                hash: chunk.normalized_hash().unwrap(),
            });
            true
        })
        .unwrap();
    RegionNode::new(entries).hash()
}

/// Assert the cache holds exactly `image` (and its content hash) under
/// `hash`.
async fn assert_cached<S: SekaiStore>(store: &S, hash: RegionHash, image: &[u8]) {
    let cache = store.cache();
    assert_eq!(cache.mca_hash(hash).await.unwrap(), Some(hash_bytes(image)));
    let mut fetched = Vec::new();
    assert!(cache.fetch_image(hash, &mut fetched).await.unwrap());
    assert_eq!(fetched, image);
}

/// Commit records history only: the region hashes it names stay uncached
/// until an update pass runs (ADR-0034).
#[tokio::test]
async fn commit_records_history_without_writing_cache_entries() {
    let (root, _guard) = tempdir("cache-commit-only");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, chunk_nbt("marker-1"))],
    );

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    let (report, _) = instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();
    assert_eq!(report.chunks, 1);

    let key = RegionKey::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
    let hash = committed_region_hash(instance.store(), report.commit, key)
        .await
        .unwrap();
    assert_eq!(instance.store().cache().mca_hash(hash).await.unwrap(), None);
}

/// The update pass before a full commit caches every fully covered
/// region, across dimensions and region families, under the hashes the
/// commit names.
#[tokio::test]
async fn update_cache_then_commit_caches_every_covered_region() {
    let (root, _guard) = tempdir("cache-full");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let files = [
        ("region/r.0.0.mca", Dimension::OVERWORLD, RegionKind::Region),
        (
            "DIM-1/region/r.0.0.mca",
            Dimension::NETHER,
            RegionKind::Region,
        ),
        ("poi/r.0.0.mca", Dimension::OVERWORLD, RegionKind::Poi),
        (
            "entities/r.0.0.mca",
            Dimension::OVERWORLD,
            RegionKind::Entities,
        ),
    ];
    // Distinct content per file: the cache is content-addressed, so equal
    // chunks would collapse to one region node across dimensions and kinds.
    for (i, (name, _, _)) in files.iter().enumerate() {
        write_region(
            world_path.join(name),
            &[(0, 0, chunk_nbt(&format!("marker-{i}")))],
        );
    }

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .update_cache(update_options(), &())
        .await
        .unwrap();
    let (report, _) = instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();
    assert_eq!(report.chunks, 4);

    for (name, dim, kind) in files {
        let key = RegionKey::new(dim, kind, 0, 0);
        let hash = committed_region_hash(instance.store(), report.commit, key)
            .await
            .unwrap_or_else(|| panic!("no committed region node for {name}"));
        let image = std::fs::read(world_path.join(name)).unwrap();
        assert_cached(instance.store(), hash, &image).await;
    }
}

/// A repeated update and commit keep the first cached image: a new header
/// timestamp changes the raw bytes but not the normalized chunk hashes,
/// so the same region node is produced and first-image-wins keeps the
/// original representative.
#[tokio::test]
async fn repeated_update_and_commit_keep_the_first_image() {
    let (root, _guard) = tempdir("cache-first-image");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let path = world_path.join("region/r.0.0.mca");
    let chunks = [(0, 0, chunk_nbt("marker-1")), (1, 0, chunk_nbt("marker-2"))];
    write_region(&path, &chunks);
    let first_image = std::fs::read(&path).unwrap();

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .update_cache(update_options(), &())
        .await
        .unwrap();
    let (first, _) = instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();
    let key = RegionKey::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
    let hash = committed_region_hash(instance.store(), first.commit, key)
        .await
        .unwrap();
    assert_cached(instance.store(), hash, &first_image).await;

    write_region_at(&path, &chunks, 7);
    let second_image = std::fs::read(&path).unwrap();
    assert_ne!(first_image, second_image);
    instance
        .world_mut(&mut world)
        .update_cache(update_options(), &())
        .await
        .unwrap();
    let (second, _) = instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();
    assert_eq!(second.new_blobs, 0);
    let again = committed_region_hash(instance.store(), second.commit, key)
        .await
        .unwrap();
    assert_eq!(hash, again);
    assert_cached(instance.store(), hash, &first_image).await;
}

/// A changed region gets a new entry under its new node hash; the old
/// entry stays, since the cache never deletes.
#[tokio::test]
async fn changed_region_gets_a_new_entry() {
    let (root, _guard) = tempdir("cache-changed");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let path = world_path.join("region/r.0.0.mca");
    write_region(&path, &[(0, 0, chunk_nbt("marker-1"))]);
    let first_image = std::fs::read(&path).unwrap();

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .update_cache(update_options(), &())
        .await
        .unwrap();
    let (first, _) = instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();
    let key = RegionKey::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
    let first_hash = committed_region_hash(instance.store(), first.commit, key)
        .await
        .unwrap();
    assert_cached(instance.store(), first_hash, &first_image).await;

    write_region(&path, &[(0, 0, chunk_nbt("marker-2"))]);
    let second_image = std::fs::read(&path).unwrap();
    instance
        .world_mut(&mut world)
        .update_cache(update_options(), &())
        .await
        .unwrap();
    let (second, _) = instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();
    let second_hash = committed_region_hash(instance.store(), second.commit, key)
        .await
        .unwrap();
    assert_ne!(first_hash, second_hash);
    assert_cached(instance.store(), second_hash, &second_image).await;
    assert_cached(instance.store(), first_hash, &first_image).await;
}

/// A partial scope merges the parent's out-of-scope chunks into the
/// region node: when the live file did not produce that node, it must not
/// be cached under it (ADR-0029 soundness). The update pass keys the file
/// under its own whole-file node, which stays distinct from the merged
/// one as long as an out-of-scope chunk changed too.
#[tokio::test]
async fn partial_scope_does_not_cache_the_merged_region() {
    let (root, _guard) = tempdir("cache-partial");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let path = world_path.join("region/r.0.0.mca");
    write_region(
        &path,
        &[(0, 0, chunk_nbt("marker-1")), (1, 0, chunk_nbt("marker-2"))],
    );
    let first_image = std::fs::read(&path).unwrap();

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .update_cache(update_options(), &())
        .await
        .unwrap();
    let (first, _) = instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();
    let key = RegionKey::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
    let first_hash = committed_region_hash(instance.store(), first.commit, key)
        .await
        .unwrap();
    assert_cached(instance.store(), first_hash, &first_image).await;

    // Change both chunks, then commit just (0,0): (1,0) is carried from
    // the parent, so the merged node is not the file's own.
    write_region(
        &path,
        &[(0, 0, chunk_nbt("marker-9")), (1, 0, chunk_nbt("marker-3"))],
    );
    let second_image = std::fs::read(&path).unwrap();
    let file_hash = file_region_hash(&path);
    instance
        .world_mut(&mut world)
        .update_cache(update_options(), &())
        .await
        .unwrap();
    let scope = Scope::Select {
        kinds: Scope::all_kinds(),
        areas: vec![(Dimension::OVERWORLD, Area::Rect(Rect::new(0, 0, 0, 0)))],
    };
    let (second, _) = instance
        .world_mut(&mut world)
        .commit(options(), scope, &())
        .await
        .unwrap();
    let second_hash = committed_region_hash(instance.store(), second.commit, key)
        .await
        .unwrap();
    assert_ne!(first_hash, second_hash);
    assert_ne!(file_hash, second_hash);
    assert_eq!(
        instance
            .store()
            .cache()
            .mca_hash(second_hash)
            .await
            .unwrap(),
        None
    );
    // The file's own node is cached soundly, and the old entry stays.
    assert_cached(instance.store(), file_hash, &second_image).await;
    assert_cached(instance.store(), first_hash, &first_image).await;
}

/// A rectangle covering the whole region file is a full cover: the
/// scoped commit names the file's own node, which the update pass cached.
#[tokio::test]
async fn fully_covered_rect_caches_the_region() {
    let (root, _guard) = tempdir("cache-rect");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let path = world_path.join("region/r.0.0.mca");
    write_region(
        &path,
        &[
            (0, 0, chunk_nbt("marker-1")),
            (31, 31, chunk_nbt("marker-2")),
        ],
    );
    let image = std::fs::read(&path).unwrap();

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .update_cache(update_options(), &())
        .await
        .unwrap();
    let scope = Scope::Select {
        kinds: Scope::all_kinds(),
        areas: vec![(Dimension::OVERWORLD, Area::Rect(Rect::region(0, 0)))],
    };
    let (report, _) = instance
        .world_mut(&mut world)
        .commit(options(), scope, &())
        .await
        .unwrap();
    let key = RegionKey::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
    let hash = committed_region_hash(instance.store(), report.commit, key)
        .await
        .unwrap();
    assert_cached(instance.store(), hash, &image).await;
}

/// The status preview writes nothing, and neither does the commit that
/// follows it: only the update pass populates the cache (ADR-0034).
#[tokio::test]
async fn status_and_commit_write_no_cache_entries() {
    let (root, _guard) = tempdir("cache-status");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let path = world_path.join("region/r.0.0.mca");
    write_region(&path, &[(0, 0, chunk_nbt("marker-1"))]);
    let image = std::fs::read(&path).unwrap();
    let expected = file_region_hash(&path);

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    let (report, _) = instance
        .world(&world)
        .status(StatusOptions::default(), Scope::World, &())
        .await
        .unwrap();
    assert!(!report.clean);
    assert_eq!(instance.list_commits().await.unwrap(), []);
    assert_eq!(
        instance.store().cache().mca_hash(expected).await.unwrap(),
        None
    );

    // The commit names the file's own node but caches nothing itself.
    let (commit, _) = instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();
    let key = RegionKey::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
    let hash = committed_region_hash(instance.store(), commit.commit, key)
        .await
        .unwrap();
    assert_eq!(hash, expected);
    assert_eq!(
        instance.store().cache().mca_hash(expected).await.unwrap(),
        None
    );

    // The update pass keys exactly the hash the commit named.
    instance
        .world_mut(&mut world)
        .update_cache(update_options(), &())
        .await
        .unwrap();
    assert_cached(instance.store(), hash, &image).await;
}

/// An empty region file holds no chunks, names no region node, and is
/// never cached.
#[tokio::test]
async fn empty_region_is_not_cached() {
    let (root, _guard) = tempdir("cache-empty");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    write_region(world_path.join("region/r.0.0.mca"), &[]);

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    let (report, _) = instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();
    assert_eq!(report.chunks, 0);
    let key = RegionKey::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
    assert!(
        committed_region_hash(instance.store(), report.commit, key)
            .await
            .is_none()
    );
    let empty = RegionNode::new(Vec::new()).hash();
    assert_eq!(
        instance.store().cache().mca_hash(empty).await.unwrap(),
        None
    );
}

/// `clean_cache` wipes every entry and reports the count; the store keeps
/// working and the next update pass rebuilds the cache, so the wipe costs
/// performance only. An empty cache cleans to zero.
#[tokio::test]
async fn clean_cache_wipes_entries_and_the_next_update_rebuilds_them() {
    let (root, _guard) = tempdir("cache-clean");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let path = world_path.join("region/r.0.0.mca");
    write_region(&path, &[(0, 0, chunk_nbt("marker-1"))]);
    let image = std::fs::read(&path).unwrap();

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    assert_eq!(instance.clean_cache().await.unwrap(), 0);
    instance
        .world_mut(&mut world)
        .update_cache(update_options(), &())
        .await
        .unwrap();
    let (first, _) = instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();
    let key = RegionKey::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
    let hash = committed_region_hash(instance.store(), first.commit, key)
        .await
        .unwrap();
    assert_cached(instance.store(), hash, &image).await;

    assert_eq!(instance.clean_cache().await.unwrap(), 1);
    assert_eq!(instance.store().cache().mca_hash(hash).await.unwrap(), None);
    assert_eq!(instance.clean_cache().await.unwrap(), 0);

    // The commit graph is untouched: the same world state commits cleanly
    // and the update pass repopulates the wiped entry.
    instance
        .world_mut(&mut world)
        .update_cache(update_options(), &())
        .await
        .unwrap();
    let (second, _) = instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();
    assert_eq!(second.new_blobs, 0);
    let again = committed_region_hash(instance.store(), second.commit, key)
        .await
        .unwrap();
    assert_eq!(hash, again);
    assert_cached(instance.store(), hash, &image).await;
}

/// The cache is derived state: wiping it costs performance only, and the
/// next update pass rebuilds every entry even without world changes.
#[tokio::test]
async fn wiped_cache_is_rebuilt_on_the_next_update() {
    let (root, _guard) = tempdir("cache-wiped");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store_dir = root.join("store");
    let store = store_dir.to_string_lossy().into_owned();
    let path = world_path.join("region/r.0.0.mca");
    write_region(&path, &[(0, 0, chunk_nbt("marker-1"))]);
    let image = std::fs::read(&path).unwrap();

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    instance
        .world_mut(&mut world)
        .update_cache(update_options(), &())
        .await
        .unwrap();
    let (first, _) = instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();
    let key = RegionKey::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
    let hash = committed_region_hash(instance.store(), first.commit, key)
        .await
        .unwrap();
    assert_cached(instance.store(), hash, &image).await;

    drop(instance);
    std::fs::remove_dir_all(store_dir.join("cache")).unwrap();
    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    assert_eq!(instance.store().cache().mca_hash(hash).await.unwrap(), None);

    instance
        .world_mut(&mut world)
        .update_cache(update_options(), &())
        .await
        .unwrap();
    let (second, _) = instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();
    assert_eq!(second.new_blobs, 0);
    let again = committed_region_hash(instance.store(), second.commit, key)
        .await
        .unwrap();
    assert_eq!(hash, again);
    assert_cached(instance.store(), hash, &image).await;
}

/// `update_cache` populates the cache from the live world without recording
/// a commit or writing a single blob: the entry is keyed by the file's own
/// region node, which a later commit of the unchanged file names exactly.
#[tokio::test]
async fn update_cache_populates_without_recording_a_commit() {
    let (root, _guard) = tempdir("cache-update-nocommit");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let path = world_path.join("region/r.0.0.mca");
    write_region(&path, &[(0, 0, chunk_nbt("marker-1"))]);
    let image = std::fs::read(&path).unwrap();
    let expected = file_region_hash(&path);

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    let (report, _) = instance
        .world_mut(&mut world)
        .update_cache(update_options(), &())
        .await
        .unwrap();
    assert_eq!(report.files, 1);
    assert_eq!(report.chunks, 1);
    assert_eq!(report.entries, 1);
    assert_eq!(report.existing, 0);
    assert_eq!(report.empty, 0);

    // Nothing was recorded, and the entry sits under the file's own node.
    assert_eq!(instance.list_commits().await.unwrap(), []);
    assert_cached(instance.store(), expected, &image).await;

    // No blob was written: the next commit stores the chunk fresh and names
    // exactly the hash the update already cached, keeping the cached image.
    let (commit, _) = instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();
    assert_eq!(commit.new_blobs, 1);
    let key = RegionKey::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
    let hash = committed_region_hash(instance.store(), commit.commit, key)
        .await
        .unwrap();
    assert_eq!(hash, expected);
    assert_cached(instance.store(), hash, &image).await;
}

/// A repeated update is idempotent: a new header timestamp changes the raw
/// bytes but not the normalized chunk hashes, so the same region node is
/// produced and first-image-wins keeps the original representative.
#[tokio::test]
async fn update_cache_is_idempotent_and_keeps_the_first_image() {
    let (root, _guard) = tempdir("cache-update-idempotent");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let path = world_path.join("region/r.0.0.mca");
    let chunks = [(0, 0, chunk_nbt("marker-1")), (1, 0, chunk_nbt("marker-2"))];
    write_region(&path, &chunks);
    let first_image = std::fs::read(&path).unwrap();
    let expected = file_region_hash(&path);

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    let (first, _) = instance
        .world_mut(&mut world)
        .update_cache(update_options(), &())
        .await
        .unwrap();
    assert_eq!(first.entries, 1);
    assert_cached(instance.store(), expected, &first_image).await;

    write_region_at(&path, &chunks, 7);
    let second_image = std::fs::read(&path).unwrap();
    assert_ne!(first_image, second_image);
    let (second, _) = instance
        .world_mut(&mut world)
        .update_cache(update_options(), &())
        .await
        .unwrap();
    assert_eq!(second.entries, 0);
    assert_eq!(second.existing, 1);
    assert_eq!(file_region_hash(&path), expected);
    assert_cached(instance.store(), expected, &first_image).await;
    assert_eq!(instance.list_commits().await.unwrap(), []);
}

/// An empty region file holds no chunks, names no region node, and is
/// counted as empty rather than cached.
#[tokio::test]
async fn update_cache_skips_empty_region_files() {
    let (root, _guard) = tempdir("cache-update-empty");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    write_region(world_path.join("region/r.0.0.mca"), &[]);

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    let (report, _) = instance
        .world_mut(&mut world)
        .update_cache(update_options(), &())
        .await
        .unwrap();
    assert_eq!(report.files, 1);
    assert_eq!(report.chunks, 0);
    assert_eq!(report.entries, 0);
    assert_eq!(report.empty, 1);
    let empty = RegionNode::new(Vec::new()).hash();
    assert_eq!(
        instance.store().cache().mca_hash(empty).await.unwrap(),
        None
    );
    assert_eq!(instance.list_commits().await.unwrap(), []);
}

/// An update visits every dimension and region family, caching each file
/// under the node a commit later names, without writing blobs or commits.
#[tokio::test]
async fn update_cache_caches_every_region_family() {
    let (root, _guard) = tempdir("cache-update-families");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let files = [
        ("region/r.0.0.mca", Dimension::OVERWORLD, RegionKind::Region),
        (
            "DIM-1/region/r.0.0.mca",
            Dimension::NETHER,
            RegionKind::Region,
        ),
        ("poi/r.0.0.mca", Dimension::OVERWORLD, RegionKind::Poi),
        (
            "entities/r.0.0.mca",
            Dimension::OVERWORLD,
            RegionKind::Entities,
        ),
    ];
    // Distinct content per file: the cache is content-addressed, so equal
    // chunks would collapse to one region node across dimensions and kinds.
    for (i, (name, _, _)) in files.iter().enumerate() {
        write_region(
            world_path.join(name),
            &[(0, 0, chunk_nbt(&format!("marker-{i}")))],
        );
    }

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    let (report, _) = instance
        .world_mut(&mut world)
        .update_cache(update_options(), &())
        .await
        .unwrap();
    assert_eq!(report.files, 4);
    assert_eq!(report.chunks, 4);
    assert_eq!(report.entries, 4);
    assert_eq!(report.empty, 0);
    assert_eq!(instance.list_commits().await.unwrap(), []);

    // The commit stores every blob fresh (the update wrote none) and names
    // the hashes the update cached; the cached images survive untouched.
    let (commit, _) = instance
        .world_mut(&mut world)
        .commit(options(), Scope::World, &())
        .await
        .unwrap();
    assert_eq!(commit.new_blobs, 4);
    for (name, dim, kind) in files {
        let key = RegionKey::new(dim, kind, 0, 0);
        let hash = committed_region_hash(instance.store(), commit.commit, key)
            .await
            .unwrap_or_else(|| panic!("no committed region node for {name}"));
        let image = std::fs::read(world_path.join(name)).unwrap();
        assert_cached(instance.store(), hash, &image).await;
    }
}
