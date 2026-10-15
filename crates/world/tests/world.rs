//! World filesystem behavior: discovery, fingerprints, swaps.

use sekai_core::{Dimension, RegionKey, RegionKind};
use sekai_world::{HostWorldTree, RegionRef, WorldTree, atomic_swap, fingerprint_file, open_image};
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
    let mut builder = sekai_anvil::RegionBuilder::new(0, 0, 0).unwrap();
    builder.stage_chunk(0, 0, &[2, 1, 2, 3]).unwrap();
    builder.image().unwrap()
}

fn write(path: impl AsRef<Path>, bytes: &[u8]) {
    std::fs::create_dir_all(path.as_ref().parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// Discovered files sorted by path, for order-independent assertions.
fn paths_of(found: &[RegionRef]) -> Vec<PathBuf> {
    let mut paths: Vec<_> = found.iter().map(|r| r.path.clone()).collect();
    paths.sort();
    paths
}

/// Every discovered file derives back to its own path: the tree resolved
/// each observed dimension directory at construction.
fn assert_derives_back(tree: &HostWorldTree, found: &[RegionRef]) {
    for r in found {
        assert_eq!(
            tree.derive_path(r.dim, r.kind, r.region_x, r.region_z)
                .unwrap(),
            r.path
        );
    }
}

/// This branch makes no exception for the vanilla trio: hash-derived
/// layouts never use the reserved codes.
fn assert_hash_ids(found: &[RegionRef]) {
    for r in found {
        assert!(
            ![Dimension::OVERWORLD, Dimension::NETHER, Dimension::END].contains(&r.dim),
            "observed dimension uses a reserved code: {}",
            r.path.display()
        );
    }
}

/// Same path, same id: reopening the root reproduces every discovered id.
fn assert_stable_ids(root: impl AsRef<Path>, found: &[RegionRef]) {
    let reopened = HostWorldTree::new(root).unwrap().discover().unwrap();
    let mut once: Vec<_> = found.iter().map(|r| (r.path.clone(), r.dim)).collect();
    let mut again: Vec<_> = reopened.iter().map(|r| (r.path.clone(), r.dim)).collect();
    once.sort();
    again.sort();
    assert_eq!(once, again);
}

/// One id per distinct dimension observed under `root`.
fn discovered_dims(root: impl AsRef<Path>) -> Vec<Dimension> {
    let mut dims: Vec<_> = HostWorldTree::new(root)
        .unwrap()
        .discover()
        .unwrap()
        .iter()
        .map(|r| r.dim)
        .collect();
    dims.sort_unstable();
    dims.dedup();
    dims
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
    let found = world.discover().unwrap();
    let overworld = world_path.join("dimensions/minecraft/overworld/region/r.2.0.mca");
    let custom = world_path.join("dimensions/aether/sky/region/r.0.1.mca");
    assert_eq!(paths_of(&found), vec![custom.clone(), overworld.clone()]);

    // Two folders, two distinct path-derived ids.
    let over = found.iter().find(|r| r.path == overworld).unwrap();
    let sky = found.iter().find(|r| r.path == custom).unwrap();
    assert_ne!(over.dim, sky.dim);

    assert_hash_ids(&found);
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
    let found = world.discover().unwrap();
    assert_eq!(
        paths_of(&found),
        vec![
            world_path.join("DIM-1/region/r.0.0.mca"),
            world_path.join("DIM1/entities/r.1.0.mca"),
            world_path.join("region/r.0.0.mca"),
        ]
    );
    let nether = found
        .iter()
        .find(|r| r.path == world_path.join("DIM-1/region/r.0.0.mca"))
        .unwrap();
    let over = found
        .iter()
        .find(|r| r.path == world_path.join("region/r.0.0.mca"))
        .unwrap();
    assert_ne!(nether.dim, over.dim);
    assert_hash_ids(&found);
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
    let found = root.discover().unwrap();
    assert_eq!(
        paths_of(&found),
        vec![
            root_path.join("world/dimensions/minecraft/overworld/region/r.0.0.mca"),
            root_path.join("world/dimensions/minecraft/the_end/region/r.0.0.mca"),
            root_path.join("world/dimensions/minecraft/the_nether/region/r.0.0.mca"),
        ]
    );
    assert_hash_ids(&found);
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
    let found = root.discover().unwrap();
    assert_eq!(
        paths_of(&found),
        vec![
            root_path.join("srv/dimensions/aeon/deep/region/r.0.1.mca"),
            root_path.join("srv/dimensions/minecraft/overworld/region/r.0.0.mca"),
            root_path.join("srv/dimensions/minecraft/the_nether/region/r.0.0.mca"),
        ]
    );
    let custom = found
        .iter()
        .find(|r| r.path == root_path.join("srv/dimensions/aeon/deep/region/r.0.1.mca"))
        .unwrap();
    let over = found
        .iter()
        .find(|r| r.path == root_path.join("srv/dimensions/minecraft/overworld/region/r.0.0.mca"))
        .unwrap();
    assert_ne!(custom.dim, over.dim);
    assert_hash_ids(&found);
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
    let found = root.discover().unwrap();
    assert_eq!(
        paths_of(&found),
        vec![
            root_path.join("world/dimensions/minecraft/overworld/region/r.0.0.mca"),
            root_path.join("world/dimensions/minecraft/the_nether/region/r.0.0.mca"),
        ]
    );
    assert_hash_ids(&found);
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
    let found = root.discover().unwrap();
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
    let custom = found.iter().find(|r| r.path == spawn).unwrap();
    let over = found
        .iter()
        .find(|r| r.path == root_path.join("world/region/r.0.0.mca"))
        .unwrap();
    assert_ne!(custom.dim, over.dim);
    assert_hash_ids(&found);
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
    let found = root.discover().unwrap();
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

    assert_eq!(discovered_dims(&root_path), discovered_dims(&legacy_path));
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

    assert_eq!(discovered_dims(&modern_path), discovered_dims(&legacy_path));
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
    let found = root.discover().unwrap();
    assert_eq!(
        paths_of(&found),
        vec![
            root_path.join("world/dimensions/minecraft/overworld/region/r.0.0.mca"),
            root_path.join("world/dimensions/minecraft/the_nether/region/r.0.0.mca"),
        ]
    );
    assert_hash_ids(&found);
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

    let found = root.discover().unwrap();
    assert_eq!(
        paths_of(&found),
        vec![
            root_path.join("dimensions/minecraft/overworld/region/r.0.0.mca"),
            root_path.join("dimensions/minecraft/the_end/region/r.0.0.mca"),
            root_path.join("dimensions/minecraft/the_nether/region/r.0.0.mca"),
        ]
    );
    assert_hash_ids(&found);
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
    let found = root.discover().unwrap();
    assert_eq!(
        paths_of(&found),
        vec![
            root_path.join("survival/dimensions/minecraft/overworld/region/r.1.0.mca"),
            root_path.join("survival/dimensions/minecraft/the_nether/region/r.0.0.mca"),
        ]
    );
    assert_hash_ids(&found);
    assert_derives_back(&root, &found);
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
    let found = root.discover().unwrap();
    assert_eq!(
        paths_of(&found),
        vec![
            root_path.join("world/dimensions/minecraft/overworld/region/r.0.0.mca"),
            root_path.join("world/dimensions/minecraft/the_nether/region/r.0.0.mca"),
        ]
    );
    assert_hash_ids(&found);
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
    let found = root.discover().unwrap();
    assert_eq!(paths_of(&found), vec![root_path.join("region/r.0.0.mca")]);
    assert_hash_ids(&found);
    assert_derives_back(&root, &found);
    cleanup(&root_path);
}

#[test]
fn duplicate_coordinate_names_resolve_deterministically() {
    // `r.0.0.mca` and `r.00.00.mca` both parse as region (0,0). The chosen
    // file must follow a fixed rule (lowest name first), never directory
    // iteration order.
    let root_path = tempdir("dup-coords");
    let root = HostWorldTree::new(&root_path).unwrap();
    let mut second = one_chunk_image();
    second[8192 + 4] = 7;
    write(root_path.join("region/r.0.0.mca"), &one_chunk_image());
    write(root_path.join("region/r.00.00.mca"), &second);

    let found = root.discover().unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, root_path.join("region/r.0.0.mca"));
    for _ in 0..8 {
        let again = root.discover().unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].path, root_path.join("region/r.0.0.mca"));
    }
    cleanup(&root_path);
}

#[test]
fn missing_world_is_an_error() {
    let world_path = tempdir("gone");
    let world = HostWorldTree::new(&world_path).unwrap();
    cleanup(&world_path);
    assert!(matches!(
        world.discover(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    ));
}

#[test]
fn derives_all_layouts() {
    let legacy = HostWorldTree::new_legacy("/w");
    assert_eq!(
        legacy
            .derive_path(Dimension::NETHER, RegionKind::REGION, -1, 2)
            .unwrap(),
        Path::new("/w/DIM-1/region/r.-1.2.mca")
    );

    let modern = HostWorldTree::new_dimensions("/w");
    assert_eq!(
        modern
            .derive_path(Dimension::END, RegionKind::POI, 0, 0)
            .unwrap(),
        Path::new("/w/dimensions/minecraft/the_end/poi/r.0.0.mca")
    );

    let bukkit = HostWorldTree::new_bukkit("/w", "srv");
    assert_eq!(
        bukkit
            .derive_path(Dimension::OVERWORLD, RegionKind::REGION, 0, 0)
            .unwrap(),
        Path::new("/w/srv/region/r.0.0.mca")
    );

    assert!(matches!(
        modern.derive_path(Dimension::new(4242), RegionKind::REGION, 0, 0),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    ));
    assert!(matches!(
        legacy.derive_path(Dimension::OVERWORLD, RegionKind::new(9), 0, 0),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    ));
}

#[test]
fn fingerprints_are_stable_and_change_sensitive() {
    let world = tempdir("fp");
    let path = world.join("region/r.0.0.mca");
    write(&path, &one_chunk_image());
    let key = RegionKey::new(Dimension::OVERWORLD, RegionKind::REGION, 0, 0);
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
    let key = RegionKey::new(Dimension::OVERWORLD, RegionKind::REGION, 0, 0);
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
    assert!(leftovers.is_empty());
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
