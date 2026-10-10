//! World filesystem behavior: discovery, fingerprints, scans, swaps.

use sekai_core::{Dimension, RegionKey, RegionKind};
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

/// Every region of every dimension, in dimension order.
fn discover(world: &HostWorldTree) -> Vec<sekai_world::RegionRef> {
    let mut regions = Vec::new();
    for dim in world.get_dims().unwrap() {
        regions.extend(world.get_regions(dim).unwrap());
    }
    regions
}

#[test]
fn discovers_legacy_and_new_layouts() {
    let world_path = tempdir("layouts");
    let image = one_chunk_image();
    // Legacy triple.
    write(world_path.join("region/r.0.0.mca"), &image);
    write(world_path.join("DIM-1/region/r.0.0.mca"), &image);
    write(world_path.join("DIM1/entities/r.1.0.mca"), &image);
    // New layout.
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

    // Layout resolution happens once, against the tree on disk.
    let world = HostWorldTree::new(&world_path).unwrap();
    let mut found = discover(&world);
    found.sort_by_key(|r| (r.dim.raw(), r.kind.raw(), r.region_x, r.region_z));
    let keys: Vec<_> = found
        .iter()
        .map(|r| (r.dim, r.kind, r.region_x, r.region_z))
        .collect();
    assert!(keys.contains(&(Dimension::OVERWORLD, RegionKind::REGION, 0, 0)));
    assert!(keys.contains(&(Dimension::NETHER, RegionKind::REGION, 0, 0)));
    assert!(keys.contains(&(Dimension::END, RegionKind::ENTITIES, 1, 0)));
    assert!(keys.contains(&(Dimension::OVERWORLD, RegionKind::REGION, 2, 0)));
    // Custom dimension resolves stably through discovery.
    let custom: Vec<_> = found
        .iter()
        .filter(|r| r.region_x == 0 && r.region_z == 1)
        .collect();
    assert_eq!(custom.len(), 1);
    assert_ne!(custom[0].dim, Dimension::OVERWORLD);
    assert_ne!(custom[0].dim, Dimension::NETHER);
    assert_ne!(custom[0].dim, Dimension::END);
    assert_eq!(found.len(), 5);
    // An existing `dimensions/` tree elects 26.1 derivation.
    assert_eq!(
        world
            .derive_path(Dimension::OVERWORLD, RegionKind::REGION, 2, 0)
            .unwrap(),
        world_path.join("dimensions/minecraft/overworld/region/r.2.0.mca")
    );
    cleanup(&world_path);
}

#[test]
fn get_dims_lists_only_occupied_dimensions() {
    let world_path = tempdir("get-dims");
    let image = one_chunk_image();
    write(world_path.join("region/r.0.0.mca"), &image);
    write(world_path.join("DIM1/region/r.1.0.mca"), &image);

    let world = HostWorldTree::new(&world_path).unwrap();
    assert_eq!(
        world.get_dims().unwrap(),
        vec![Dimension::OVERWORLD, Dimension::END]
    );
    let over = world.get_regions(Dimension::OVERWORLD).unwrap();
    assert_eq!(over.len(), 1);
    assert_eq!(over[0].path, world_path.join("region/r.0.0.mca"));
    assert!(world.get_regions(Dimension::NETHER).unwrap().is_empty());
    cleanup(&world_path);
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
    let found = discover(&world);
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
    let found = discover(&root);
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
    let found = discover(&root);
    assert_eq!(found.len(), 4);
    let dims: Vec<_> = found.iter().map(|r| r.dim).collect();
    assert!(dims.contains(&Dimension::OVERWORLD));
    assert!(dims.contains(&Dimension::NETHER));
    let hashed: Vec<_> = found
        .iter()
        .filter(|r| {
            r.dim != Dimension::OVERWORLD && r.dim != Dimension::NETHER && r.dim != Dimension::END
        })
        .collect();
    assert_eq!(hashed.len(), 2);
    assert_ne!(hashed[0].dim, hashed[1].dim);
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
        root.derive_path(hashed[0].dim, RegionKind::REGION, 0, 0),
        Err(sekai_world::WorldError::UnknownRegionPath { .. })
    ));
    cleanup(&root_path);
}

#[test]
fn trio_wins_over_conversion_leftovers() {
    // A stale root-level DIM-1 next to a live Bukkit trio: the trio is live
    // server data, the leftover is pre-migration residue.
    let root_path = tempdir("stale");
    let mut stale = one_chunk_image();
    stale[8192 + 4] = 9;
    let mut live = one_chunk_image();
    live[8192 + 4] = 7;
    write(root_path.join("DIM-1/region/r.0.0.mca"), &stale);
    write(root_path.join("world/region/r.1.0.mca"), &one_chunk_image());
    write(root_path.join("world_nether/DIM-1/region/r.0.0.mca"), &live);

    let root = HostWorldTree::new(&root_path).unwrap();
    let found = discover(&root);
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
        discover(root)
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

    let over: Vec<_> = discover(&root)
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
    let found = discover(&root);
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
    // The leftover trio is still discoverable, under its own namespace.
    assert_eq!(found.len(), 4);
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

    let found = discover(&root);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, root_path.join("region/r.0.0.mca"));
    for _ in 0..8 {
        let again = discover(&root);
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].path, root_path.join("region/r.0.0.mca"));
    }
    cleanup(&root_path);
}

#[test]
fn nested_vanilla_copy_gets_hashed_codes() {
    // A full vanilla world copied under the root keeps working under hashed
    // namespaces instead of colliding with the outer namespaces.
    let root_path = tempdir("nested");
    let root = HostWorldTree::new(&root_path).unwrap();
    let image = one_chunk_image();
    write(root_path.join("region/r.0.0.mca"), &image);
    write(root_path.join("old/region/r.0.0.mca"), &image);
    write(root_path.join("old/DIM-1/region/r.0.0.mca"), &image);
    let found = discover(&root);
    assert_eq!(found.len(), 3);
    assert!(
        found
            .iter()
            .any(|r| r.dim == Dimension::OVERWORLD && r.path == root_path.join("region/r.0.0.mca"))
    );
    let hashed = found
        .iter()
        .filter(|r| r.dim != Dimension::OVERWORLD)
        .count();
    assert_eq!(hashed, 2);
    cleanup(&root_path);
}

#[test]
fn missing_world_is_an_error() {
    let world_path = tempdir("gone");
    let world = HostWorldTree::new(&world_path).unwrap();
    cleanup(&world_path);
    assert!(matches!(
        world.get_dims(),
        Err(sekai_world::WorldError::Io { .. })
    ));
    assert!(matches!(
        world.get_regions(Dimension::OVERWORLD),
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
