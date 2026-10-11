//! World filesystem behavior: discovery, fingerprints, scans, swaps.

use sekai_core::{Dimension, RegionKey, RegionKind, resolve_custom_dimension};
use sekai_world::{
    HostWorldTree, WorldTree, atomic_swap, fingerprint_file, open_image, scan_world,
};
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

#[test]
fn dimensions_tree_elects_vanilla_layout() {
    // A 26.1 vanilla world keeps every dimension under `dimensions/`; legacy
    // files still sitting at the root are pre-migration leftovers and must
    // not be scanned once exactly one layout is elected.
    let world_path = tempdir("layouts");
    let image = one_chunk_image();
    // Leftovers from the pre-26.1 layout.
    write(world_path.join("region/r.0.0.mca"), &image);
    write(world_path.join("DIM-1/region/r.0.0.mca"), &image);
    write(world_path.join("DIM1/entities/r.1.0.mca"), &image);
    // The elected 26.1 tree.
    write(
        world_path.join("dimensions/minecraft/overworld/region/r.2.0.mca"),
        &image,
    );
    write(
        world_path.join("dimensions/aether/sky/region/r.0.1.mca"),
        &image,
    );
    // Foreign files are ignored.
    write(world_path.join("region/notes.txt"), b"nope");

    let world = HostWorldTree::new(&world_path).unwrap();
    let found = world.discover().unwrap();
    assert_eq!(
        found.len(),
        2,
        "only the elected layout is discovered: {found:?}"
    );

    let overworld: Vec<_> = found
        .iter()
        .filter(|r| r.dim == Dimension::OVERWORLD)
        .collect();
    assert_eq!(overworld.len(), 1);
    assert_eq!(
        overworld[0].path,
        world_path.join("dimensions/minecraft/overworld/region/r.2.0.mca")
    );
    assert_eq!((overworld[0].region_x, overworld[0].region_z), (2, 0));

    // Datapack/plugin dimensions hash their `<namespace>:<name>` pair.
    let custom: Vec<_> = found
        .iter()
        .filter(|r| r.region_x == 0 && r.region_z == 1)
        .collect();
    assert_eq!(custom.len(), 1);
    assert_eq!(custom[0].dim, resolve_custom_dimension("aether:sky"));
    assert_eq!(
        custom[0].path,
        world_path.join("dimensions/aether/sky/region/r.0.1.mca")
    );

    // Derivation targets the elected 26.1 tree.
    assert_eq!(
        world
            .derive_path(Dimension::OVERWORLD, RegionKind::REGION, 2, 0)
            .unwrap(),
        world_path.join("dimensions/minecraft/overworld/region/r.2.0.mca")
    );
    cleanup(&world_path);
}

#[test]
fn discovers_legacy_vanilla_layout() {
    let world_path = tempdir("legacy-vanilla");
    let image = one_chunk_image();
    write(world_path.join("region/r.0.0.mca"), &image);
    write(world_path.join("DIM-1/region/r.0.0.mca"), &image);
    write(world_path.join("DIM1/entities/r.1.0.mca"), &image);

    let world = HostWorldTree::new(&world_path).unwrap();
    let found = world.discover().unwrap();
    assert_eq!(found.len(), 3);
    let at = |dim, kind| {
        found
            .iter()
            .find(|r| r.dim == dim && r.kind == kind)
            .unwrap()
            .path
            .clone()
    };
    assert_eq!(
        at(Dimension::OVERWORLD, RegionKind::REGION),
        world_path.join("region/r.0.0.mca")
    );
    assert_eq!(
        at(Dimension::NETHER, RegionKind::REGION),
        world_path.join("DIM-1/region/r.0.0.mca")
    );
    assert_eq!(
        at(Dimension::END, RegionKind::ENTITIES),
        world_path.join("DIM1/entities/r.1.0.mca")
    );
    assert_eq!(
        world
            .derive_path(Dimension::NETHER, RegionKind::REGION, 0, 0)
            .unwrap(),
        world_path.join("DIM-1/region/r.0.0.mca")
    );
    cleanup(&world_path);
}

#[test]
fn discovers_paper_26_1_layout_under_level_name() {
    // Paper 26.1+ keeps splitting worlds by `level-name`, but the world
    // folder now holds the 26.1 `dimensions/` tree. Pre-migration leftovers
    // next to it must not be scanned.
    let root_path = tempdir("paper-dimensions");
    let image = one_chunk_image();
    write(
        root_path.join("server.properties"),
        b"level-name=survival\n",
    );
    write(
        root_path.join("survival/dimensions/minecraft/overworld/region/r.0.0.mca"),
        &image,
    );
    write(
        root_path.join("survival/dimensions/minecraft/the_nether/region/r.0.0.mca"),
        &image,
    );
    write(
        root_path.join("survival/dimensions/aether/sky/region/r.0.1.mca"),
        &image,
    );
    write(root_path.join("survival/region/r.9.9.mca"), &image);
    write(
        root_path.join("survival_nether/DIM-1/region/r.9.9.mca"),
        &image,
    );

    let root = HostWorldTree::new(&root_path).unwrap();
    let found = root.discover().unwrap();
    assert_eq!(
        found.len(),
        3,
        "only the elected layout is discovered: {found:?}"
    );
    assert!(found.iter().any(|r| {
        r.dim == Dimension::OVERWORLD
            && r.path == root_path.join("survival/dimensions/minecraft/overworld/region/r.0.0.mca")
    }));
    assert!(found.iter().any(|r| {
        r.dim == Dimension::NETHER
            && r.path == root_path.join("survival/dimensions/minecraft/the_nether/region/r.0.0.mca")
    }));
    assert!(found.iter().any(|r| {
        r.dim == resolve_custom_dimension("aether:sky")
            && r.path == root_path.join("survival/dimensions/aether/sky/region/r.0.1.mca")
    }));
    assert_eq!(
        root.derive_path(Dimension::OVERWORLD, RegionKind::REGION, 0, 0)
            .unwrap(),
        root_path.join("survival/dimensions/minecraft/overworld/region/r.0.0.mca")
    );
    cleanup(&root_path);
}

#[test]
fn discovers_bukkit_nesting() {
    let world_path = tempdir("bukkit");
    let image = one_chunk_image();
    write(world_path.join("world/region/r.0.0.mca"), &image);
    write(
        world_path.join("world_nether/DIM-1/region/r.0.0.mca"),
        &image,
    );
    write(
        world_path.join("world_the_end/DIM1/region/r.0.0.mca"),
        &image,
    );

    let world = HostWorldTree::new(&world_path).unwrap();
    let found = world.discover().unwrap();
    assert_eq!(found.len(), 3);
    let at = |dim, kind| {
        found
            .iter()
            .find(|r| r.dim == dim && r.kind == kind)
            .unwrap()
            .path
            .clone()
    };
    assert_eq!(
        at(Dimension::OVERWORLD, RegionKind::REGION),
        world_path.join("world/region/r.0.0.mca")
    );
    assert_eq!(
        at(Dimension::NETHER, RegionKind::REGION),
        world_path.join("world_nether/DIM-1/region/r.0.0.mca")
    );
    assert_eq!(
        at(Dimension::END, RegionKind::REGION),
        world_path.join("world_the_end/DIM1/region/r.0.0.mca")
    );
    // The elected trio drives derivation.
    assert_eq!(
        world
            .derive_path(Dimension::OVERWORLD, RegionKind::REGION, 0, 0)
            .unwrap(),
        world_path.join("world/region/r.0.0.mca")
    );
    assert_eq!(
        world
            .derive_path(Dimension::NETHER, RegionKind::REGION, 0, 0)
            .unwrap(),
        world_path.join("world_nether/DIM-1/region/r.0.0.mca")
    );
    cleanup(&world_path);
}

#[test]
fn discovers_custom_level_name_trio() {
    // `level-name: srv` servers use srv/srv_nether/srv_the_end.
    let root_path = tempdir("levelname");
    let image = one_chunk_image();
    write(root_path.join("srv/region/r.0.0.mca"), &image);
    write(root_path.join("srv_nether/DIM-1/region/r.0.0.mca"), &image);
    write(root_path.join("srv_the_end/DIM1/region/r.0.0.mca"), &image);

    let root = HostWorldTree::new(&root_path).unwrap();
    let found = root.discover().unwrap();
    assert_eq!(found.len(), 3);
    assert!(
        found
            .iter()
            .any(|r| r.dim == Dimension::OVERWORLD
                && r.path == root_path.join("srv/region/r.0.0.mca"))
    );
    assert!(found.iter().any(|r| r.dim == Dimension::NETHER
        && r.path == root_path.join("srv_nether/DIM-1/region/r.0.0.mca")));
    // Derivation follows the custom base.
    assert_eq!(
        root.derive_path(Dimension::NETHER, RegionKind::REGION, 0, 0)
            .unwrap(),
        root_path.join("srv_nether/DIM-1/region/r.0.0.mca")
    );
    cleanup(&root_path);
}

#[test]
fn multiverse_worlds_never_collide() {
    // Main trio keeps vanilla codes; extra world folders hash theirs, so
    // same-environment worlds cannot silently share coordinates.
    let root_path = tempdir("multiverse");
    let image = one_chunk_image();
    write(root_path.join("world/region/r.0.0.mca"), &image);
    write(
        root_path.join("world_nether/DIM-1/region/r.0.0.mca"),
        &image,
    );
    write(root_path.join("sky/region/r.0.0.mca"), &image);
    write(root_path.join("sky_nether/DIM-1/region/r.0.0.mca"), &image);

    let root = HostWorldTree::new(&root_path).unwrap();
    let found = root.discover().unwrap();
    assert_eq!(found.len(), 4);
    let dims: Vec<_> = found.iter().map(|r| r.dim).collect();
    assert!(dims.contains(&Dimension::OVERWORLD));
    assert!(dims.contains(&Dimension::NETHER));
    // The plugin trio is keyed by its base folder and the logical dimension
    // names, so it can never alias the main world's coordinates.
    let sky: Vec<_> = found
        .iter()
        .filter(|r| r.dim == resolve_custom_dimension("sky:overworld"))
        .collect();
    assert_eq!(sky.len(), 1);
    assert_eq!(sky[0].path, root_path.join("sky/region/r.0.0.mca"));
    let sky_nether: Vec<_> = found
        .iter()
        .filter(|r| r.dim == resolve_custom_dimension("sky:the_nether"))
        .collect();
    assert_eq!(sky_nether.len(), 1);
    assert_eq!(
        sky_nether[0].path,
        root_path.join("sky_nether/DIM-1/region/r.0.0.mca")
    );
    // No two entries share a key: nothing was silently dropped.
    let mut keys: Vec<_> = found
        .iter()
        .map(|r| (r.dim, r.kind, r.region_x, r.region_z))
        .collect();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), 4);
    // Hashed dims are not derivable; rollback uses discovered folders.
    assert!(matches!(
        root.derive_path(
            resolve_custom_dimension("sky:overworld"),
            RegionKind::REGION,
            0,
            0
        ),
        Err(sekai_world::WorldError::UnknownRegionPath { .. })
    ));
    cleanup(&root_path);
}

#[test]
fn root_level_leftovers_are_not_scanned() {
    // A stale root-level DIM-1 next to a live Bukkit trio: exactly one
    // layout is respected, so the leftover is ignored entirely instead of
    // being merged or overwritten.
    let root_path = tempdir("stale");
    let mut stale = one_chunk_image();
    stale[8192 + 4] = 9;
    let mut live = one_chunk_image();
    live[8192 + 4] = 7;
    write(root_path.join("DIM-1/region/r.0.0.mca"), &stale);
    write(root_path.join("world/region/r.1.0.mca"), &one_chunk_image());
    write(root_path.join("world_nether/DIM-1/region/r.0.0.mca"), &live);

    let root = HostWorldTree::new(&root_path).unwrap();
    let found = root.discover().unwrap();
    assert_eq!(found.len(), 2, "the leftover must stay invisible");
    let nether: Vec<_> = found
        .iter()
        .filter(|r| r.dim == Dimension::NETHER)
        .collect();
    assert_eq!(nether.len(), 1);
    assert_eq!(open_image(&nether[0].path).unwrap()[8192 + 4], 7);
    cleanup(&root_path);
}

#[test]
fn empty_overworld_keeps_the_trio_namespaces() {
    // A freshly created (or briefly emptied) overworld has a `region/`
    // directory but no files in it. Namespaces must not flip to hashed
    // codes just because the overworld holds nothing right now.
    let root_path = tempdir("empty-overworld");
    let image = one_chunk_image();
    std::fs::create_dir_all(root_path.join("world/region")).unwrap();
    write(
        root_path.join("world_nether/DIM-1/region/r.0.0.mca"),
        &image,
    );
    write(
        root_path.join("world_the_end/DIM1/region/r.0.0.mca"),
        &image,
    );

    // An empty `region/` directory already elects the trio, so resolving
    // now keeps the vanilla namespaces even though no file exists yet.
    let root = HostWorldTree::new(&root_path).unwrap();

    // The empty overworld still elects the trio: derivation targets the
    // `world/` folder rather than hashed codes.
    assert_eq!(
        root.derive_path(Dimension::OVERWORLD, RegionKind::REGION, 0, 0)
            .unwrap(),
        root_path.join("world/region/r.0.0.mca")
    );

    // The same siblings keep their vanilla codes whether or not the
    // overworld holds files.
    let nether = |root: &HostWorldTree| {
        root.discover()
            .unwrap()
            .into_iter()
            .find(|r| r.dim == Dimension::NETHER)
            .map(|r| r.path)
    };
    let empty = nether(&root);
    assert_eq!(
        empty,
        Some(root_path.join("world_nether/DIM-1/region/r.0.0.mca"))
    );

    write(root_path.join("world/region/r.0.0.mca"), &image);
    assert_eq!(
        root.derive_path(Dimension::OVERWORLD, RegionKind::REGION, 0, 0)
            .unwrap(),
        root_path.join("world/region/r.0.0.mca")
    );
    assert_eq!(nether(&root), empty);

    let over: Vec<_> = root
        .discover()
        .unwrap()
        .into_iter()
        .filter(|r| r.dim == Dimension::OVERWORLD)
        .collect();
    assert_eq!(over.len(), 1);
    assert_eq!(over[0].path, root_path.join("world/region/r.0.0.mca"));
    cleanup(&root_path);
}

#[test]
fn level_name_wins_over_a_leftover_world_folder() {
    // `level-name=survival` with a stale `world/` trio still on disk: the
    // server loads `survival`, so its files must keep the vanilla codes and
    // the leftover must not be promoted to OVERWORLD.
    let root_path = tempdir("levelname-leftover");
    let live = one_chunk_image();
    let mut stale = one_chunk_image();
    stale[8192 + 4] = 9;
    write(
        root_path.join("server.properties"),
        b"motd=x\nlevel-name=survival\n",
    );
    write(root_path.join("survival/region/r.1.0.mca"), &live);
    write(
        root_path.join("survival_nether/DIM-1/region/r.0.0.mca"),
        &live,
    );
    write(root_path.join("world/region/r.9.9.mca"), &stale);
    write(
        root_path.join("world_nether/DIM-1/region/r.0.0.mca"),
        &stale,
    );

    let root = HostWorldTree::new(&root_path).unwrap();
    // `level-name` wins: derivation targets the survival folder.
    assert_eq!(
        root.derive_path(Dimension::OVERWORLD, RegionKind::REGION, 1, 0)
            .unwrap(),
        root_path.join("survival/region/r.1.0.mca")
    );
    let found = root.discover().unwrap();
    let over: Vec<_> = found
        .iter()
        .filter(|r| r.dim == Dimension::OVERWORLD)
        .collect();
    assert_eq!(over.len(), 1);
    assert_eq!(over[0].path, root_path.join("survival/region/r.1.0.mca"));
    let nether: Vec<_> = found
        .iter()
        .filter(|r| r.dim == Dimension::NETHER)
        .collect();
    assert_eq!(nether.len(), 1);
    assert_eq!(
        nether[0].path,
        root_path.join("survival_nether/DIM-1/region/r.0.0.mca")
    );
    // The leftover trio still works as a plugin world under hashed
    // `<base>:<logical dimension>` keys, without stealing vanilla codes.
    assert_eq!(found.len(), 4);
    let leftover_over: Vec<_> = found
        .iter()
        .filter(|r| r.dim == resolve_custom_dimension("world:overworld"))
        .collect();
    assert_eq!(leftover_over.len(), 1);
    assert_eq!(
        leftover_over[0].path,
        root_path.join("world/region/r.9.9.mca")
    );
    let leftover_nether: Vec<_> = found
        .iter()
        .filter(|r| r.dim == resolve_custom_dimension("world:the_nether"))
        .collect();
    assert_eq!(leftover_nether.len(), 1);
    assert_eq!(
        leftover_nether[0].path,
        root_path.join("world_nether/DIM-1/region/r.0.0.mca")
    );
    cleanup(&root_path);
}

#[test]
fn stray_dimensions_dir_does_not_hijack_a_bukkit_root() {
    // A `dimensions/` directory left at a Bukkit container root (migration
    // residue, a plugin) must not make the root look like a 26.1 vanilla
    // world: rollback would then restore into a tree the server ignores.
    let root_path = tempdir("stray-dimensions");
    let image = one_chunk_image();
    write(root_path.join("world/region/r.0.0.mca"), &image);
    write(
        root_path.join("world_nether/DIM-1/region/r.0.0.mca"),
        &image,
    );
    std::fs::create_dir_all(root_path.join("dimensions/minecraft/the_nether")).unwrap();

    let root = HostWorldTree::new(&root_path).unwrap();
    // The elected trio wins over the stray `dimensions/` tree.
    assert_eq!(
        root.derive_path(Dimension::NETHER, RegionKind::REGION, 0, 0)
            .unwrap(),
        root_path.join("world_nether/DIM-1/region/r.0.0.mca")
    );
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
fn nested_vanilla_copy_gets_hashed_codes() {
    // A full vanilla world copied under the root keeps working as a plugin
    // world under hashed `<base>:<logical dimension>` namespaces instead of
    // colliding with the outer namespaces.
    let root_path = tempdir("nested");
    let root = HostWorldTree::new(&root_path).unwrap();
    let image = one_chunk_image();
    write(root_path.join("region/r.0.0.mca"), &image);
    write(root_path.join("old/region/r.0.0.mca"), &image);
    write(root_path.join("old/DIM-1/region/r.0.0.mca"), &image);
    let found = root.discover().unwrap();
    assert_eq!(found.len(), 3);
    assert!(
        found
            .iter()
            .any(|r| r.dim == Dimension::OVERWORLD && r.path == root_path.join("region/r.0.0.mca"))
    );
    let at = |dim| {
        let entries: Vec<_> = found.iter().filter(|r| r.dim == dim).collect();
        assert_eq!(entries.len(), 1);
        entries[0].path.clone()
    };
    assert_eq!(
        at(resolve_custom_dimension("old:overworld")),
        root_path.join("old/region/r.0.0.mca")
    );
    assert_eq!(
        at(resolve_custom_dimension("old:the_nether")),
        root_path.join("old/DIM-1/region/r.0.0.mca")
    );
    cleanup(&root_path);
}

#[test]
fn missing_world_is_an_error() {
    let world_path = tempdir("gone");
    let world = HostWorldTree::new(&world_path).unwrap();
    cleanup(&world_path);
    assert!(matches!(
        world.discover(),
        Err(sekai_world::WorldError::Io { .. })
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
        Err(sekai_world::WorldError::UnknownRegionPath { .. })
    ));
    assert!(matches!(
        legacy.derive_path(Dimension::OVERWORLD, RegionKind::new(9), 0, 0),
        Err(sekai_world::WorldError::UnknownRegionPath { .. })
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
fn scan_counts_chunks_without_writing() {
    let world = tempdir("scan");
    write(world.join("region/r.0.0.mca"), &one_chunk_image());
    write(world.join("region/r.1.0.mca"), &[0u8; 8192]);
    write(world.join("region/r.2.0.mca"), &[]);
    let report = scan_world(&world).unwrap();
    let mut entries = report.entries;
    entries.sort_by_key(|e| e.region_x);
    assert!(report.skipped.is_empty());
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].chunks, 1);
    assert_eq!(entries[0].file_bytes % 4096, 0);
    assert_eq!(entries[0].content_hash.len(), 64);
    assert_eq!(entries[1].chunks, 0);
    assert_eq!(entries[2].chunks, 0);
    assert_eq!(entries[2].file_bytes, 0);
    let timings = report.timings;
    assert!(timings.total >= timings.discover + timings.read + timings.parse);
    cleanup(&world);
}

/// A corrupt file must be reported, not silently dropped: an inventory
/// that undercounts is worse than one that fails.
#[test]
fn scan_reports_unreadable_files() {
    let world = tempdir("scan-corrupt");
    write(world.join("region/r.0.0.mca"), &one_chunk_image());
    // Truncated below the 8 KiB header: unreadable, but discoverable.
    write(world.join("region/r.1.0.mca"), &[7u8; 100]);
    let report = scan_world(&world).unwrap();
    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.skipped.len(), 1);
    assert!(report.skipped[0].path.ends_with("r.1.0.mca"));
    assert!(
        !report.skipped[0].reason.is_empty(),
        "a skip must carry a reason"
    );
    cleanup(&world);
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
