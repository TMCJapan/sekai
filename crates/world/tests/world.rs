//! World filesystem behavior: discovery, fingerprints, swaps.

use sekai_core::{Dimension, RegionKey, RegionKind};
use sekai_world::{HostWorldTree, WorldTree, atomic_swap, fingerprint_file, open_image};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sekai-world-{}-{}-{}",
        name,
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

/// Minimal one-chunk region image bytes.
fn one_chunk_image() -> Vec<u8> {
    one_chunk_image_at(0)
}

/// The same image with a different timestamp: two files that differ by
/// content while both stay valid region images.
fn one_chunk_image_at(timestamp: u32) -> Vec<u8> {
    let mut builder = sekai_anvil::RegionBuilder::new(0, 0, timestamp).unwrap();
    builder.stage_chunk(0, 0, &[2, 1, 2, 3]).unwrap();
    builder.image().unwrap()
}

fn write(path: impl AsRef<Path>, bytes: &[u8]) {
    std::fs::create_dir_all(path.as_ref().parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// Every region of the world as its discovered key and on-disk path:
/// dimensions in ascending raw-code order (`get_dims`), and within each
/// dimension in `region`, `entities`, `poi` order (`get_regions`).
fn discover(world: &HostWorldTree) -> Vec<(RegionKey, PathBuf)> {
    let mut regions = Vec::new();
    for dim in world.get_dims().unwrap() {
        for kind in RegionKind::ALL {
            regions.extend(world.get_regions(dim, kind).unwrap());
        }
    }
    regions
}

/// Discovered files sorted by path, for order-independent assertions.
fn paths_of(found: &[(RegionKey, PathBuf)]) -> Vec<PathBuf> {
    let mut paths: Vec<_> = found.iter().map(|(_, path)| path.clone()).collect();
    paths.sort();
    paths
}

/// Key of the discovered file at `path`.
fn key_at(found: &[(RegionKey, PathBuf)], path: impl AsRef<Path>) -> RegionKey {
    found
        .iter()
        .find(|(_, p)| p.as_path() == path.as_ref())
        .unwrap()
        .0
}

/// Canonical fixtures: every discovered key derives back to its own path,
/// because the tree resolved each observed dimension directory at
/// construction. Non-canonical spellings are covered separately.
fn assert_derives_back(tree: &HostWorldTree, found: &[(RegionKey, PathBuf)]) {
    for (key, path) in found {
        assert_eq!(&tree.derive_path(*key).unwrap(), path);
    }
}

/// Same path, same id: reopening the root reproduces every discovered id.
fn assert_stable_ids(root: impl AsRef<Path>, found: &[(RegionKey, PathBuf)]) {
    let reopened = discover(&HostWorldTree::new(root).unwrap());
    let mut once: Vec<_> = found
        .iter()
        .map(|(key, path)| (path.clone(), *key))
        .collect();
    let mut again: Vec<_> = reopened
        .iter()
        .map(|(key, path)| (path.clone(), *key))
        .collect();
    once.sort();
    again.sort();
    assert_eq!(once, again);
}

/// One id per distinct dimension observed under `root`.
#[cfg(unix)]
fn discovered_dims(root: impl AsRef<Path>) -> Vec<Dimension> {
    let mut dims: Vec<_> = discover(&HostWorldTree::new(root).unwrap())
        .iter()
        .map(|(key, _)| key.dim)
        .collect();
    dims.sort_unstable();
    dims.dedup();
    dims
}

/// Every discovered dimension mapped to its region file's path. The id
/// fixtures below hold exactly one file per dimension.
fn dim_paths(root: impl AsRef<Path>) -> BTreeMap<Dimension, PathBuf> {
    let mut paths = BTreeMap::new();
    for (key, path) in discover(&HostWorldTree::new(root).unwrap()) {
        assert!(
            paths.insert(key.dim, path).is_none(),
            "fixtures hold one region file per dimension (duplicate: {key:?})"
        );
    }
    paths
}

#[test]
fn observes_a_single_layout_per_root() {
    // A root holding both a 26.1 `dimensions/` tree and legacy folders is
    // resolved once: the dimensions tree is the observed layout and the
    // legacy files stay out of discovery.
    let world_path = tempdir("layouts");
    let image = one_chunk_image();
    write(world_path.join("region/r.0.0.mca"), &image);
    write(world_path.join("DIM-1/region/r.0.0.mca"), &image);
    write(world_path.join("DIM1/entities/r.1.0.mca"), &image);
    write(
        world_path.join("dimensions/minecraft/overworld/region/r.2.0.mca"),
        &image,
    );
    write(
        world_path.join("dimensions/aether/sky/region/r.0.1.mca"),
        &image,
    );
    write(world_path.join("region/notes.txt"), b"nope");

    let world = HostWorldTree::new(&world_path).unwrap();
    let found = discover(&world);
    let overworld = world_path.join("dimensions/minecraft/overworld/region/r.2.0.mca");
    let custom = world_path.join("dimensions/aether/sky/region/r.0.1.mca");
    assert_eq!(paths_of(&found), vec![custom.clone(), overworld.clone()]);

    // Two folders, two distinct path-derived ids.
    assert_ne!(key_at(&found, &overworld).dim, key_at(&found, &custom).dim);

    assert_derives_back(&world, &found);
    assert_stable_ids(&world_path, &found);
    cleanup(&world_path);
}

#[test]
fn get_dims_lists_every_layout_dimension() {
    let world_path = tempdir("get-dims");
    let image = one_chunk_image();
    write(world_path.join("region/r.0.0.mca"), &image);
    write(world_path.join("DIM1/region/r.1.0.mca"), &image);

    let world = HostWorldTree::new(&world_path).unwrap();
    // Every layout dimension is listed - the absent nether directory
    // included; it simply discovers no regions.
    let dims = world.get_dims().unwrap();
    assert_eq!(dims.len(), 3);
    for dim in [Dimension::OVERWORLD, Dimension::NETHER, Dimension::END] {
        assert!(dims.contains(&dim), "missing layout dimension {dim:?}");
    }
    // The trait contract is "ordered by code": pin the ascending raw-code
    // order of the trio explicitly instead of checking self-sortedness.
    let mut expected = [Dimension::OVERWORLD, Dimension::NETHER, Dimension::END];
    expected.sort_unstable_by_key(|dim| dim.raw());
    assert_eq!(dims, expected, "dimensions are ordered by raw code");
    let over = world
        .get_regions(Dimension::OVERWORLD, RegionKind::Region)
        .unwrap();
    assert_eq!(over.len(), 1);
    assert_eq!(over[0].1, world_path.join("region/r.0.0.mca"));
    assert_eq!(
        world
            .get_regions(Dimension::NETHER, RegionKind::Region)
            .unwrap(),
        [] as [(RegionKey, PathBuf); 0]
    );
    cleanup(&world_path);
}

#[cfg(unix)]
#[test]
fn non_utf8_dimension_names_stay_discovered() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    // Unix directory names are raw bytes: neither a non-UTF-8 namespace nor
    // a non-UTF-8 dimension folder may be dropped from the tree.
    let world_path = tempdir("non-utf8");
    let image = one_chunk_image();
    let overworld = world_path.join("dimensions/minecraft/overworld/region/r.9.9.mca");
    let namespace = world_path
        .join("dimensions")
        .join(OsStr::from_bytes(b"ae\xffther"));
    let dimension = world_path
        .join("dimensions")
        .join("aether")
        .join(OsStr::from_bytes(b"sky-\xff"));
    write(&overworld, &image);
    let namespace_region = namespace.join("sky/region");
    // macOS rejects file names that are not valid UTF-8 (EILSEQ), so the
    // fixture only exists where the file system admits raw bytes; the
    // byte-keying itself stays unit-tested in `sekai-anvil`.
    if std::fs::create_dir_all(&namespace_region).is_err() {
        cleanup(&world_path);
        return;
    }
    write(namespace_region.join("r.0.0.mca"), &image);
    write(dimension.join("region/r.0.1.mca"), &image);

    let world = HostWorldTree::new(&world_path).unwrap();
    let found = discover(&world);
    assert_eq!(
        paths_of(&found),
        vec![
            dimension.join("region/r.0.1.mca"),
            namespace.join("sky/region/r.0.0.mca"),
            overworld,
        ]
    );
    // Distinct ids: the byte-named dimensions and the vanilla trio do not
    // collapse into one another.
    assert_eq!(discovered_dims(&world_path).len(), 3);
    assert_derives_back(&world, &found);
    assert_stable_ids(&world_path, &found);
    cleanup(&world_path);
}

#[test]
fn discovers_a_legacy_single_folder_world() {
    let world_path = tempdir("legacy");
    let image = one_chunk_image();
    write(world_path.join("region/r.0.0.mca"), &image);
    write(world_path.join("DIM-1/region/r.0.0.mca"), &image);
    write(world_path.join("DIM1/entities/r.1.0.mca"), &image);
    write(world_path.join("region/notes.txt"), b"nope");

    let world = HostWorldTree::new(&world_path).unwrap();
    let found = discover(&world);
    assert_eq!(
        paths_of(&found),
        vec![
            world_path.join("DIM-1/region/r.0.0.mca"),
            world_path.join("DIM1/entities/r.1.0.mca"),
            world_path.join("region/r.0.0.mca"),
        ]
    );
    let nether = key_at(&found, world_path.join("DIM-1/region/r.0.0.mca"));
    let over = key_at(&found, world_path.join("region/r.0.0.mca"));
    assert_ne!(nether.dim, over.dim);
    assert_eq!(over.dim, Dimension::OVERWORLD);
    assert_eq!(nether.dim, Dimension::NETHER);
    assert_derives_back(&world, &found);
    assert_stable_ids(&world_path, &found);
    cleanup(&world_path);
}

#[test]
fn discovers_a_bukkit_level_name_world() {
    // `level-name=world`: the observed layout is the 26.1 tree nested
    // under the level-name folder.
    let root_path = tempdir("bukkit");
    let image = one_chunk_image();
    write(root_path.join("server.properties"), b"level-name=world\n");
    write(
        root_path.join("world/dimensions/minecraft/overworld/region/r.0.0.mca"),
        &image,
    );
    write(
        root_path.join("world/dimensions/minecraft/the_nether/region/r.0.0.mca"),
        &image,
    );
    write(
        root_path.join("world/dimensions/minecraft/the_end/region/r.0.0.mca"),
        &image,
    );

    let root = HostWorldTree::new(&root_path).unwrap();
    let found = discover(&root);
    assert_eq!(
        paths_of(&found),
        vec![
            root_path.join("world/dimensions/minecraft/overworld/region/r.0.0.mca"),
            root_path.join("world/dimensions/minecraft/the_end/region/r.0.0.mca"),
            root_path.join("world/dimensions/minecraft/the_nether/region/r.0.0.mca"),
        ]
    );
    assert_derives_back(&root, &found);
    assert_stable_ids(&root_path, &found);
    cleanup(&root_path);
}

#[test]
fn discovers_a_custom_level_name_world() {
    // `level-name=srv`: the observed layout follows the configured base,
    // including its custom dimensions.
    let root_path = tempdir("levelname");
    let image = one_chunk_image();
    write(
        root_path.join("server.properties"),
        b"motd=x\nlevel-name=srv\n",
    );
    write(
        root_path.join("srv/dimensions/minecraft/overworld/region/r.0.0.mca"),
        &image,
    );
    write(
        root_path.join("srv/dimensions/minecraft/the_nether/region/r.0.0.mca"),
        &image,
    );
    write(
        root_path.join("srv/dimensions/aeon/deep/region/r.0.1.mca"),
        &image,
    );

    let root = HostWorldTree::new(&root_path).unwrap();
    let found = discover(&root);
    assert_eq!(
        paths_of(&found),
        vec![
            root_path.join("srv/dimensions/aeon/deep/region/r.0.1.mca"),
            root_path.join("srv/dimensions/minecraft/overworld/region/r.0.0.mca"),
            root_path.join("srv/dimensions/minecraft/the_nether/region/r.0.0.mca"),
        ]
    );
    let custom = key_at(
        &found,
        root_path.join("srv/dimensions/aeon/deep/region/r.0.1.mca"),
    );
    let over = key_at(
        &found,
        root_path.join("srv/dimensions/minecraft/overworld/region/r.0.0.mca"),
    );
    assert_ne!(custom.dim, over.dim);
    assert_derives_back(&root, &found);
    cleanup(&root_path);
}

#[test]
fn other_world_folders_are_not_observed() {
    // A Multiverse-style sibling world is a separate tree; only the
    // level-name world belongs to this one.
    let root_path = tempdir("multiverse");
    let image = one_chunk_image();
    write(root_path.join("server.properties"), b"level-name=world\n");
    write(
        root_path.join("world/dimensions/minecraft/overworld/region/r.0.0.mca"),
        &image,
    );
    write(
        root_path.join("world/dimensions/minecraft/the_nether/region/r.0.0.mca"),
        &image,
    );
    write(root_path.join("sky/region/r.0.0.mca"), &image);
    write(root_path.join("sky/DIM-1/region/r.0.0.mca"), &image);

    let root = HostWorldTree::new(&root_path).unwrap();
    let found = discover(&root);
    assert_eq!(
        paths_of(&found),
        vec![
            root_path.join("world/dimensions/minecraft/overworld/region/r.0.0.mca"),
            root_path.join("world/dimensions/minecraft/the_nether/region/r.0.0.mca"),
        ]
    );
    cleanup(&root_path);
}

#[test]
fn discovers_a_legacy_bukkit_world() {
    // `level-name=world`: the fixed split folders plus a datapack
    // dimension in its own sibling world folder, with the data one level
    // deeper in `dimensions/kingdoms/spawn`. A sibling folder without a
    // `dimensions/` tree is a plugin world, not a dimension.
    let root_path = tempdir("bukkit-legacy");
    let image = one_chunk_image();
    write(root_path.join("server.properties"), b"level-name=world\n");
    write(root_path.join("world/region/r.0.0.mca"), &image);
    write(
        root_path.join("world_nether/DIM-1/region/r.0.0.mca"),
        &image,
    );
    write(
        root_path.join("world_the_end/DIM1/entities/r.1.0.mca"),
        &image,
    );
    write(
        root_path.join("world_kingdoms_spawn/dimensions/kingdoms/spawn/region/r.0.1.mca"),
        &image,
    );
    write(root_path.join("world_backup/region/r.9.9.mca"), &image);

    let root = HostWorldTree::new(&root_path).unwrap();
    let found = discover(&root);
    let spawn = root_path.join("world_kingdoms_spawn/dimensions/kingdoms/spawn/region/r.0.1.mca");
    assert_eq!(
        paths_of(&found),
        vec![
            root_path.join("world/region/r.0.0.mca"),
            spawn.clone(),
            root_path.join("world_nether/DIM-1/region/r.0.0.mca"),
            root_path.join("world_the_end/DIM1/entities/r.1.0.mca"),
        ]
    );
    let custom = key_at(&found, &spawn);
    let over = key_at(&found, root_path.join("world/region/r.0.0.mca"));
    assert_ne!(custom.dim, over.dim);
    assert_derives_back(&root, &found);
    assert_stable_ids(&root_path, &found);
    cleanup(&root_path);
}

#[test]
fn vanilla_dimensions_stay_out_of_a_legacy_bukkit_world() {
    // A `world/dimensions/` tree is the pre-migration vanilla location:
    // the server moves it into a sibling world folder on startup and reads
    // only the moved location, so the residue stays out of discovery.
    let root_path = tempdir("bukkit-legacy-residue");
    let image = one_chunk_image();
    write(root_path.join("server.properties"), b"level-name=world\n");
    write(root_path.join("world/region/r.0.0.mca"), &image);
    write(
        root_path.join("world_nether/DIM-1/region/r.0.0.mca"),
        &image,
    );
    write(
        root_path.join("world/dimensions/kingdoms/spawn/region/r.0.1.mca"),
        &image,
    );

    let root = HostWorldTree::new(&root_path).unwrap();
    let found = discover(&root);
    assert_eq!(
        paths_of(&found),
        vec![
            root_path.join("world/region/r.0.0.mca"),
            root_path.join("world_nether/DIM-1/region/r.0.0.mca"),
        ]
    );
    assert_derives_back(&root, &found);
    cleanup(&root_path);
}

#[test]
fn legacy_bukkit_keeps_vanilla_legacy_dimension_ids() {
    // The split layout shares the single-folder layout's identities (the
    // trio and every `<namespace>:<path>` key), so a world keeps one
    // history when it moves between vanilla and Bukkit trees.
    let root_path = tempdir("bukkit-legacy-ids");
    let legacy_path = tempdir("vanilla-legacy-ids");
    let image = one_chunk_image();
    write(root_path.join("server.properties"), b"level-name=world\n");
    for dir in [
        "world/region",
        "world_nether/DIM-1/region",
        "world_the_end/DIM1/region",
        "world_kingdoms_spawn/dimensions/kingdoms/spawn/region",
    ] {
        write(root_path.join(dir).join("r.0.0.mca"), &image);
    }
    for dir in [
        "region",
        "DIM-1/region",
        "DIM1/region",
        "dimensions/kingdoms/spawn/region",
    ] {
        write(legacy_path.join(dir).join("r.0.0.mca"), &image);
    }

    let custom = sekai_anvil::custom_dimension_id("kingdoms:spawn");
    assert_eq!(
        dim_paths(&root_path),
        BTreeMap::from([
            (
                Dimension::OVERWORLD,
                root_path.join("world/region/r.0.0.mca")
            ),
            (
                Dimension::NETHER,
                root_path.join("world_nether/DIM-1/region/r.0.0.mca")
            ),
            (
                Dimension::END,
                root_path.join("world_the_end/DIM1/region/r.0.0.mca")
            ),
            (
                custom,
                root_path.join("world_kingdoms_spawn/dimensions/kingdoms/spawn/region/r.0.0.mca")
            ),
        ])
    );
    assert_eq!(
        dim_paths(&legacy_path),
        BTreeMap::from([
            (Dimension::OVERWORLD, legacy_path.join("region/r.0.0.mca")),
            (
                Dimension::NETHER,
                legacy_path.join("DIM-1/region/r.0.0.mca")
            ),
            (Dimension::END, legacy_path.join("DIM1/region/r.0.0.mca")),
            (
                custom,
                legacy_path.join("dimensions/kingdoms/spawn/region/r.0.0.mca")
            ),
        ])
    );
    cleanup(&root_path);
    cleanup(&legacy_path);
}

#[test]
fn modern_tree_keeps_legacy_dimension_ids() {
    // The 26.1 `dimensions/` tree and the single-folder layout name the
    // same dimensions (`minecraft:overworld`, `minecraft:the_nether`,
    // `minecraft:the_end`) plus every `<namespace>:<path>` key, so a world
    // keeps one history across a migration in either direction.
    let modern_path = tempdir("vanilla-26.1-ids");
    let legacy_path = tempdir("vanilla-legacy-ids-modern");
    let image = one_chunk_image();
    for dir in [
        "dimensions/minecraft/overworld/region",
        "dimensions/minecraft/the_nether/region",
        "dimensions/minecraft/the_end/region",
        "dimensions/kingdoms/spawn/region",
    ] {
        write(modern_path.join(dir).join("r.0.0.mca"), &image);
    }
    for dir in [
        "region",
        "DIM-1/region",
        "DIM1/region",
        "dimensions/kingdoms/spawn/region",
    ] {
        write(legacy_path.join(dir).join("r.0.0.mca"), &image);
    }

    let custom = sekai_anvil::custom_dimension_id("kingdoms:spawn");
    assert_eq!(
        dim_paths(&modern_path),
        BTreeMap::from([
            (
                Dimension::OVERWORLD,
                modern_path.join("dimensions/minecraft/overworld/region/r.0.0.mca")
            ),
            (
                Dimension::NETHER,
                modern_path.join("dimensions/minecraft/the_nether/region/r.0.0.mca")
            ),
            (
                Dimension::END,
                modern_path.join("dimensions/minecraft/the_end/region/r.0.0.mca")
            ),
            (
                custom,
                modern_path.join("dimensions/kingdoms/spawn/region/r.0.0.mca")
            ),
        ])
    );
    assert_eq!(
        dim_paths(&legacy_path),
        BTreeMap::from([
            (Dimension::OVERWORLD, legacy_path.join("region/r.0.0.mca")),
            (
                Dimension::NETHER,
                legacy_path.join("DIM-1/region/r.0.0.mca")
            ),
            (Dimension::END, legacy_path.join("DIM1/region/r.0.0.mca")),
            (
                custom,
                legacy_path.join("dimensions/kingdoms/spawn/region/r.0.0.mca")
            ),
        ])
    );
    cleanup(&modern_path);
    cleanup(&legacy_path);
}

#[test]
fn legacy_leftovers_stay_out_of_a_bukkit_layout() {
    // Conversion residue at the server root or inside the level-name
    // folder is not part of the observed 26.1 layout.
    let root_path = tempdir("bukkit-leftovers");
    let image = one_chunk_image();
    write(root_path.join("server.properties"), b"level-name=world\n");
    write(
        root_path.join("world/dimensions/minecraft/overworld/region/r.0.0.mca"),
        &image,
    );
    write(
        root_path.join("world/dimensions/minecraft/the_nether/region/r.0.0.mca"),
        &image,
    );
    write(root_path.join("world/region/r.9.9.mca"), &image);
    write(root_path.join("DIM-1/region/r.9.9.mca"), &image);

    let root = HostWorldTree::new(&root_path).unwrap();
    let found = discover(&root);
    assert_eq!(
        paths_of(&found),
        vec![
            root_path.join("world/dimensions/minecraft/overworld/region/r.0.0.mca"),
            root_path.join("world/dimensions/minecraft/the_nether/region/r.0.0.mca"),
        ]
    );
    assert_derives_back(&root, &found);
    cleanup(&root_path);
}

#[test]
fn empty_dimension_directories_stay_collected() {
    // Dimension directories are resolved at construction, so an empty
    // (freshly created or briefly emptied) overworld still belongs to the
    // layout and picks up files written later.
    let root_path = tempdir("empty-dim");
    let image = one_chunk_image();
    std::fs::create_dir_all(root_path.join("dimensions/minecraft/overworld/region")).unwrap();
    write(
        root_path.join("dimensions/minecraft/the_nether/region/r.0.0.mca"),
        &image,
    );
    write(
        root_path.join("dimensions/minecraft/the_end/region/r.0.0.mca"),
        &image,
    );

    let root = HostWorldTree::new(&root_path).unwrap();
    write(
        root_path.join("dimensions/minecraft/overworld/region/r.0.0.mca"),
        &image,
    );

    let found = discover(&root);
    assert_eq!(
        paths_of(&found),
        vec![
            root_path.join("dimensions/minecraft/overworld/region/r.0.0.mca"),
            root_path.join("dimensions/minecraft/the_end/region/r.0.0.mca"),
            root_path.join("dimensions/minecraft/the_nether/region/r.0.0.mca"),
        ]
    );
    assert_derives_back(&root, &found);
    assert_stable_ids(&root_path, &found);
    cleanup(&root_path);
}

#[test]
fn level_name_wins_over_a_leftover_world_folder() {
    // `level-name=survival` with a stale `world/` tree still on disk: the
    // server loads survival, so only its layout is observed.
    let root_path = tempdir("levelname-leftover");
    let image = one_chunk_image();
    write(
        root_path.join("server.properties"),
        b"motd=x\nlevel-name=survival\n",
    );
    write(
        root_path.join("survival/dimensions/minecraft/overworld/region/r.1.0.mca"),
        &image,
    );
    write(
        root_path.join("survival/dimensions/minecraft/the_nether/region/r.0.0.mca"),
        &image,
    );
    write(
        root_path.join("world/dimensions/minecraft/overworld/region/r.9.9.mca"),
        &image,
    );
    write(
        root_path.join("world/dimensions/minecraft/the_nether/region/r.0.0.mca"),
        &image,
    );

    let root = HostWorldTree::new(&root_path).unwrap();
    let found = discover(&root);
    assert_eq!(
        paths_of(&found),
        vec![
            root_path.join("survival/dimensions/minecraft/overworld/region/r.1.0.mca"),
            root_path.join("survival/dimensions/minecraft/the_nether/region/r.0.0.mca"),
        ]
    );
    assert_derives_back(&root, &found);
    cleanup(&root_path);
}

#[test]
fn an_escaping_level_name_stays_out_of_the_layout() {
    // `server.properties` is untrusted: a value like `../escape` must not
    // point the layout outside the observed root, even when the target
    // happens to exist on disk.
    let base_path = tempdir("levelname-escape");
    let root_path = base_path.join("root");
    let image = one_chunk_image();
    write(
        root_path.join("server.properties"),
        b"level-name=../escape\n",
    );
    write(
        root_path.join("dimensions/minecraft/overworld/region/r.0.0.mca"),
        &image,
    );
    write(base_path.join("escape/region/r.9.9.mca"), &image);

    let root = HostWorldTree::new(&root_path).unwrap();
    let found = discover(&root);
    assert_eq!(
        paths_of(&found),
        vec![root_path.join("dimensions/minecraft/overworld/region/r.0.0.mca")]
    );
    assert_derives_back(&root, &found);
    cleanup(&base_path);
}

#[test]
fn an_unlistable_kind_directory_is_an_error() {
    // A kind directory that cannot be listed must not silently downgrade
    // the layout: the world would open as a different tree - or an empty
    // one - and later backups would miss its data.
    let world_path = tempdir("kinds-unlistable");
    write(world_path.join("region"), b"not a directory");

    assert!(matches!(
        HostWorldTree::new(&world_path),
        Err(sekai_world::WorldError::Io { .. })
    ));
    cleanup(&world_path);
}

#[cfg(unix)]
#[test]
fn a_permission_denied_kind_directory_is_an_error() {
    use std::os::unix::fs::PermissionsExt;
    let world_path = tempdir("kinds-denied");
    let region = world_path.join("region");
    write(region.join("r.0.0.mca"), &one_chunk_image());
    std::fs::set_permissions(&region, std::fs::Permissions::from_mode(0o000)).unwrap();

    let result = HostWorldTree::new(&world_path);
    // A user that bypasses mode bits (running as root) cannot make the
    // fixture unreadable; the expectation only holds where the mode applies.
    let denied = std::fs::read_dir(&region).is_err();

    std::fs::set_permissions(&region, std::fs::Permissions::from_mode(0o755)).unwrap();
    if denied {
        assert!(matches!(result, Err(sekai_world::WorldError::Io { .. })));
    }
    cleanup(&world_path);
}

#[cfg(unix)]
#[test]
fn a_broken_entry_does_not_hide_a_bukkit_trio() {
    // The overworld's `region/` holds a dangling link and its `entities/`
    // holds a real file: the broken entry is skipped exactly as discovery
    // skips it, so the trio is still recognized and nothing is lost.
    let root_path = tempdir("bukkit-broken-entry");
    let image = one_chunk_image();
    write(root_path.join("server.properties"), b"level-name=world\n");
    std::fs::create_dir_all(root_path.join("world/region")).unwrap();
    std::os::unix::fs::symlink(
        root_path.join("missing-target"),
        root_path.join("world/region/r.0.0.mca"),
    )
    .unwrap();
    write(root_path.join("world/entities/r.0.0.mca"), &image);

    let root = HostWorldTree::new(&root_path).unwrap();
    let found = discover(&root);
    assert_eq!(
        paths_of(&found),
        vec![root_path.join("world/entities/r.0.0.mca")]
    );
    assert_derives_back(&root, &found);
    cleanup(&root_path);
}

#[test]
fn a_bukkit_trio_without_server_properties_is_still_elected() {
    // No `server.properties` (deleted, or the server never wrote one):
    // the split trio is elected from structure alone.
    let root_path = tempdir("bukkit-no-props");
    let image = one_chunk_image();
    write(root_path.join("world/region/r.0.0.mca"), &image);
    write(
        root_path.join("world_nether/DIM-1/region/r.0.0.mca"),
        &image,
    );
    write(
        root_path.join("world_the_end/DIM1/entities/r.1.0.mca"),
        &image,
    );

    let root = HostWorldTree::new(&root_path).unwrap();
    let found = discover(&root);
    assert_eq!(
        paths_of(&found),
        vec![
            root_path.join("world/region/r.0.0.mca"),
            root_path.join("world_nether/DIM-1/region/r.0.0.mca"),
            root_path.join("world_the_end/DIM1/entities/r.1.0.mca"),
        ]
    );
    assert_derives_back(&root, &found);
    assert_stable_ids(&root_path, &found);
    cleanup(&root_path);
}

#[test]
fn a_26_1_bukkit_tree_without_server_properties_is_still_elected() {
    // A migrated 26.1 tree without `server.properties`: the base is
    // elected by its nested `dimensions/` folder.
    let root_path = tempdir("bukkit-26.1-no-props");
    let image = one_chunk_image();
    write(
        root_path.join("world/dimensions/minecraft/overworld/region/r.0.0.mca"),
        &image,
    );
    write(
        root_path.join("world/dimensions/minecraft/the_nether/region/r.0.0.mca"),
        &image,
    );

    let root = HostWorldTree::new(&root_path).unwrap();
    let found = discover(&root);
    assert_eq!(
        paths_of(&found),
        vec![
            root_path.join("world/dimensions/minecraft/overworld/region/r.0.0.mca"),
            root_path.join("world/dimensions/minecraft/the_nether/region/r.0.0.mca"),
        ]
    );
    assert_derives_back(&root, &found);
    assert_stable_ids(&root_path, &found);
    cleanup(&root_path);
}

#[test]
fn stray_dimensions_dir_does_not_hijack_a_bukkit_root() {
    // A `dimensions/` tree left at the server root (migration residue, a
    // plugin) must not displace the level-name world's own layout.
    let root_path = tempdir("stray-dimensions");
    let image = one_chunk_image();
    write(root_path.join("server.properties"), b"level-name=world\n");
    write(
        root_path.join("world/dimensions/minecraft/overworld/region/r.0.0.mca"),
        &image,
    );
    write(
        root_path.join("world/dimensions/minecraft/the_nether/region/r.0.0.mca"),
        &image,
    );
    write(
        root_path.join("dimensions/minecraft/the_nether/region/r.9.9.mca"),
        &image,
    );

    let root = HostWorldTree::new(&root_path).unwrap();
    let found = discover(&root);
    assert_eq!(
        paths_of(&found),
        vec![
            root_path.join("world/dimensions/minecraft/overworld/region/r.0.0.mca"),
            root_path.join("world/dimensions/minecraft/the_nether/region/r.0.0.mca"),
        ]
    );
    cleanup(&root_path);
}

#[test]
fn nested_world_folders_are_not_observed() {
    // A full vanilla world copied under the observed one stays out:
    // single-layout observation never walks into nested folders.
    let root_path = tempdir("nested");
    let image = one_chunk_image();
    write(root_path.join("region/r.0.0.mca"), &image);
    write(root_path.join("old/region/r.0.0.mca"), &image);
    write(root_path.join("old/DIM-1/region/r.0.0.mca"), &image);

    let root = HostWorldTree::new(&root_path).unwrap();
    let found = discover(&root);
    assert_eq!(paths_of(&found), vec![root_path.join("region/r.0.0.mca")]);
    assert_derives_back(&root, &found);
    cleanup(&root_path);
}

#[test]
fn duplicate_coordinate_names_resolve_to_one_canonical_key() {
    // `r.0.0.mca` and `r.00.00.mca` both parse as region (0,0). Discovery
    // reports the coordinate once, and its derived path is the canonical
    // spelling, independent of directory iteration order.
    let root_path = tempdir("dup-coords");
    let root = HostWorldTree::new(&root_path).unwrap();
    // The two spellings hold different content, so the winner is pinned by
    // its bytes and not only by its name.
    let canonical = one_chunk_image();
    let alternate = one_chunk_image_at(1);
    write(root_path.join("region/r.0.0.mca"), &canonical);
    write(root_path.join("region/r.00.00.mca"), &alternate);

    let found = discover(&root);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].1, root_path.join("region/r.0.0.mca"));
    assert_eq!(
        found[0].0,
        RegionKey::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0)
    );
    assert_ne!(
        sekai_anvil::content_hash(&canonical),
        sekai_anvil::content_hash(&alternate),
        "the two spellings must differ for the winner to mean anything"
    );
    assert_eq!(
        fingerprint_file(&found[0].1, found[0].0)
            .unwrap()
            .content_hash,
        sekai_anvil::content_hash(&canonical),
        "the surviving file is the canonically spelled one, byte for byte"
    );
    for _ in 0..8 {
        let again = discover(&root);
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].1, root_path.join("region/r.0.0.mca"));
    }
    cleanup(&root_path);
}

#[test]
fn non_canonical_region_name_keeps_its_discovered_path() {
    // `r.00.00.mca` parses as region (0,0). Discovery must report the file
    // that exists, while `derive_path` keeps naming the canonical spelling
    // used for output targets.
    let world_path = tempdir("non-canonical");
    let path = world_path.join("region/r.00.00.mca");
    write(&path, &one_chunk_image());

    let world = HostWorldTree::new(&world_path).unwrap();
    let regions = world
        .get_regions(Dimension::OVERWORLD, RegionKind::Region)
        .unwrap();
    assert_eq!(regions.len(), 1);
    let (key, discovered) = &regions[0];
    assert_eq!((key.rx, key.rz), (0, 0));
    assert_eq!(discovered, &path);
    assert_eq!(
        world.derive_path(*key).unwrap(),
        world_path.join("region/r.0.0.mca")
    );
    cleanup(&world_path);
}

#[cfg(unix)]
#[test]
fn unresolvable_entries_are_skipped_not_fatal() {
    // A dangling symlink inside a kind directory must not abort the whole
    // discovery: one unreadable entry cannot make the world un-backupable.
    let world_path = tempdir("dangling");
    write(world_path.join("region/r.0.0.mca"), &one_chunk_image());
    std::os::unix::fs::symlink(
        world_path.join("missing-target"),
        world_path.join("region/r.1.0.mca"),
    )
    .unwrap();

    let world = HostWorldTree::new(&world_path).unwrap();
    let found = discover(&world);
    assert_eq!(paths_of(&found), vec![world_path.join("region/r.0.0.mca")]);
    cleanup(&world_path);
}

#[test]
fn regions_are_ordered_by_kind_and_coordinates() {
    // Region enumeration reports keys in ascending coordinates within a
    // kind, not file-name order (`r.10.0.mca` sorts before `r.2.0.mca`
    // lexicographically), and the kind walk follows `RegionKind::ALL`.
    let world_path = tempdir("region-order");
    let image = one_chunk_image();
    for name in ["r.10.0.mca", "r.2.0.mca", "r.-1.0.mca"] {
        write(world_path.join("region").join(name), &image);
    }
    write(world_path.join("entities/r.5.0.mca"), &image);
    write(world_path.join("poi/r.6.0.mca"), &image);

    let world = HostWorldTree::new(&world_path).unwrap();
    let expected = [
        (RegionKind::Region, vec![(-1, 0), (2, 0), (10, 0)]),
        (RegionKind::Entities, vec![(5, 0)]),
        (RegionKind::Poi, vec![(6, 0)]),
    ];
    for (kind, coords) in expected {
        let keys = world.get_regions(Dimension::OVERWORLD, kind).unwrap();
        let actual: Vec<_> = keys.iter().map(|(key, _)| (key.rx, key.rz)).collect();
        assert_eq!(actual, coords, "kind {kind:?}");
    }
    let kinds: Vec<_> = discover(&world)
        .into_iter()
        .map(|(key, _)| key.kind)
        .collect();
    assert_eq!(
        kinds,
        [
            RegionKind::Region,
            RegionKind::Region,
            RegionKind::Region,
            RegionKind::Entities,
            RegionKind::Poi,
        ]
    );
    cleanup(&world_path);
}

#[test]
fn missing_world_is_an_error() {
    let world_path = tempdir("gone");
    let world = HostWorldTree::new(&world_path).unwrap();
    cleanup(&world_path);
    // Dimension enumeration fails loudly: a vanished world must not read
    // as empty.
    assert!(matches!(
        world.get_dims(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    ));
    // Region enumeration is a plain directory read, and a missing root
    // holds no regions.
    assert_eq!(
        world
            .get_regions(Dimension::OVERWORLD, RegionKind::Region)
            .unwrap(),
        [] as [(RegionKey, PathBuf); 0]
    );
}

/// Export builds `new_legacy`/`new_dimensions`/`new_bukkit` trees for
/// roots that do not exist yet, so region enumeration must answer empty
/// instead of failing while the root is still missing.
#[test]
fn output_trees_serve_no_regions_before_creation() {
    let root = tempdir("output-missing");
    let missing = root.join("not-yet");
    for tree in [
        HostWorldTree::new_legacy(&missing),
        HostWorldTree::new_dimensions(&missing),
        HostWorldTree::new_bukkit(&missing, "srv"),
    ] {
        assert_eq!(
            tree.get_regions(Dimension::OVERWORLD, RegionKind::Region)
                .unwrap(),
            [] as [(RegionKey, PathBuf); 0]
        );
    }
    cleanup(&root);
}

#[test]
fn derives_all_layouts() {
    let legacy = HostWorldTree::new_legacy("/w");
    assert_eq!(
        legacy
            .derive_path(RegionKey::new(Dimension::NETHER, RegionKind::Region, -1, 2))
            .unwrap(),
        Path::new("/w/DIM-1/region/r.-1.2.mca")
    );

    let modern = HostWorldTree::new_dimensions("/w");
    assert_eq!(
        modern
            .derive_path(RegionKey::new(Dimension::END, RegionKind::Poi, 0, 0))
            .unwrap(),
        Path::new("/w/dimensions/minecraft/the_end/poi/r.0.0.mca")
    );

    let bukkit = HostWorldTree::new_bukkit("/w", "srv");
    assert_eq!(
        bukkit
            .derive_path(RegionKey::new(
                Dimension::OVERWORLD,
                RegionKind::Region,
                0,
                0
            ))
            .unwrap(),
        Path::new("/w/srv/region/r.0.0.mca")
    );

    let err = modern
        .derive_path(RegionKey::new(
            Dimension::new(4242),
            RegionKind::Region,
            0,
            0,
        ))
        .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    assert_eq!(
        err.to_string(),
        format!(
            "cannot derive region path for dim 4242, kind {}, r.0.0",
            RegionKind::Region.raw()
        )
    );
}

/// Export derives each stored region's path through a constructor tree
/// (`new_legacy`, `new_dimensions`, `new_bukkit`), so the constructors have
/// to key the vanilla trio exactly as discovery does for the same layout.
#[test]
fn output_trees_share_discovered_ids() {
    let image = one_chunk_image();

    let legacy_path = tempdir("output-ids-legacy");
    write(legacy_path.join("region/r.0.0.mca"), &image);
    write(legacy_path.join("DIM-1/region/r.-1.2.mca"), &image);
    write(legacy_path.join("DIM1/poi/r.1.0.mca"), &image);

    let modern_path = tempdir("output-ids-26.1");
    write(
        modern_path.join("dimensions/minecraft/the_nether/region/r.2.0.mca"),
        &image,
    );

    let bukkit_path = tempdir("output-ids-bukkit");
    write(bukkit_path.join("server.properties"), b"level-name=srv\n");
    write(bukkit_path.join("srv/region/r.0.0.mca"), &image);
    write(
        bukkit_path.join("srv_nether/DIM-1/region/r.3.0.mca"),
        &image,
    );

    for (root, out) in [
        (&legacy_path, HostWorldTree::new_legacy(&legacy_path)),
        (&modern_path, HostWorldTree::new_dimensions(&modern_path)),
        (&bukkit_path, HostWorldTree::new_bukkit(&bukkit_path, "srv")),
    ] {
        let found = discover(&HostWorldTree::new(root).unwrap());
        assert_ne!(found, [] as [(RegionKey, PathBuf); 0]);
        assert_derives_back(&out, &found);
    }

    cleanup(legacy_path);
    cleanup(modern_path);
    cleanup(bukkit_path);
}

#[test]
fn fingerprints_are_stable_and_change_sensitive() {
    let world = tempdir("fp");
    let path = world.join("region/r.0.0.mca");
    write(&path, &one_chunk_image());
    let key = RegionKey::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
    let a = fingerprint_file(&path, key).unwrap();
    assert_eq!(a, fingerprint_file(&path, key).unwrap());
    // Same size, new header content: must mismatch.
    let mut other = one_chunk_image();
    other[0] ^= 0xFF;
    // Keep the size identical so only the header signal fires.
    assert_eq!(other.len() as u64, a.size);
    write(&path, &other);
    assert_ne!(
        a.content_hash,
        fingerprint_file(&path, key).unwrap().content_hash
    );
    // Missing file is an I/O error, not a fingerprint.
    cleanup(&world);
    assert!(matches!(
        fingerprint_file(&path, key),
        Err(sekai_world::WorldError::Io { .. })
    ));
}

/// The whole point of hashing content: a payload edit past the location
/// table, with the file's size *and* mtime preserved, must still be seen.
/// `cp -p`, `rsync -t`, `tar -x`, a ZFS/Btrfs snapshot rollback, or any
/// coarse-mtime filesystem produce exactly this file.
#[test]
fn fingerprint_sees_a_payload_edit_with_preserved_size_and_mtime() {
    let world = tempdir("fp-payload");
    let path = world.join("region/r.0.0.mca");
    let key = RegionKey::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
    // A fixed timestamp both writes are stamped with, so mtime cannot be the
    // signal that catches the edit.
    let stamp = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);

    let original = one_chunk_image();
    write(&path, &original);
    set_mtime(&path, stamp);
    let before = fingerprint_file(&path, key).unwrap();

    // Rewrite the chunk payload in place, keeping the compressed length, so
    // the location table and the file size stay byte-identical.
    let mut changed = original.clone();
    for byte in &mut changed[original.len() - 4..] {
        *byte ^= 0xFF;
    }
    assert_eq!(changed.len(), original.len());
    write(&path, &changed);
    set_mtime(&path, stamp);

    let after = fingerprint_file(&path, key).unwrap();
    assert_eq!(after.size, before.size, "size must be identical");
    assert_eq!(after.mtime_ms, before.mtime_ms, "mtime must be identical");
    assert_ne!(
        after.content_hash, before.content_hash,
        "a same-size payload edit under a preserved mtime must be detected"
    );

    // And therefore the pair does not match, which is what plan_backup asks
    // before skipping a file.
    let stored = sekai_core::RegionStateEntry {
        fingerprint: before,
        snapshot_id: sekai_core::SnapshotId(1),
    };
    assert!(
        !after.matches_state(&stored),
        "the file must not be treated as unchanged"
    );
    cleanup(&world);
}

fn set_mtime(path: impl AsRef<Path>, time: std::time::SystemTime) {
    let file = std::fs::File::options()
        .write(true)
        .open(path)
        .expect("file is writable");
    let times = std::fs::FileTimes::new().set_modified(time);
    file.set_times(times).expect("mtime is settable");
}

#[test]
fn atomic_swap_replaces_and_cleans_up() {
    let world = tempdir("swap");
    let target = world.join("region/r.0.0.mca");
    write(&target, b"old");
    atomic_swap(&target, &one_chunk_image()).unwrap();
    assert_eq!(open_image(&target).unwrap(), one_chunk_image());
    // Overwrite again; no temp leftovers may remain.
    atomic_swap(&target, b"new").unwrap();
    assert_eq!(open_image(&target).unwrap(), b"new");
    let leftovers: Vec<_> = std::fs::read_dir(world.join("region"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp-"))
        .collect();
    assert_eq!(leftovers, [] as [std::string::String; 0]);
    // Missing parents are created.
    let nested = world.join("DIM-1/region/r.5.5.mca");
    atomic_swap(&nested, b"data").unwrap();
    assert_eq!(open_image(&nested).unwrap(), b"data");
    // Missing source is an I/O error.
    cleanup(&world);
    assert!(matches!(
        open_image(&target),
        Err(sekai_world::WorldError::Io { .. })
    ));
}
#[cfg(unix)]
#[test]
fn atomic_swap_preserves_target_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let world = tempdir("swap-mode");
    let target = world.join("region/r.0.0.mca");
    write(&target, b"old");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o444)).unwrap();

    atomic_swap(&target, &one_chunk_image()).unwrap();

    // The rename replaces the inode, so the mode has to be carried over or a
    // hardened file silently becomes writable again.
    let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o444);
    cleanup(&world);
}
