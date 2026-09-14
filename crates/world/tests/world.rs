//! World filesystem behavior: discovery, fingerprints, scans, swaps.

use sekai_core::{Dimension, RegionKey, RegionKind};
use sekai_world::{
    LayoutFlavor, atomic_swap, derive_path, detect_flavor, discover, fingerprint_file, open_image,
    scan_world,
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

fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

/// Minimal one-chunk region image bytes.
fn one_chunk_image() -> Vec<u8> {
    let mut builder = sekai_anvil::RegionBuilder::new(0, 0, 0).unwrap();
    builder.stage_chunk(0, 0, &[2, 1, 2, 3]).unwrap();
    builder.image().unwrap()
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn discovers_legacy_and_new_layouts() {
    let world = tempdir("layouts");
    let image = one_chunk_image();
    // Legacy triple.
    write(&world.join("region/r.0.0.mca"), &image);
    write(&world.join("DIM-1/region/r.0.0.mca"), &image);
    write(&world.join("DIM1/entities/r.1.0.mca"), &image);
    // New layout.
    write(
        &world.join("dimensions/minecraft/overworld/region/r.2.0.mca"),
        &image,
    );
    write(
        &world.join("dimensions/aether/sky/region/r.0.1.mca"),
        &image,
    );
    // Foreign files are ignored.
    write(&world.join("region/notes.txt"), b"nope");

    let mut found = discover(&world).unwrap();
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
    assert_eq!(detect_flavor(&world).unwrap(), LayoutFlavor::New);
    cleanup(&world);
}

#[test]
fn discovers_bukkit_nesting() {
    let world = tempdir("bukkit");
    let image = one_chunk_image();
    write(&world.join("world/region/r.0.0.mca"), &image);
    write(&world.join("world_nether/DIM-1/region/r.0.0.mca"), &image);
    write(&world.join("world_the_end/DIM1/region/r.0.0.mca"), &image);
    let found = discover(&world).unwrap();
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
        world.join("world/region/r.0.0.mca")
    );
    assert_eq!(
        at(Dimension::NETHER, RegionKind::REGION),
        world.join("world_nether/DIM-1/region/r.0.0.mca")
    );
    assert_eq!(
        at(Dimension::END, RegionKind::REGION),
        world.join("world_the_end/DIM1/region/r.0.0.mca")
    );
    assert_eq!(
        detect_flavor(&world).unwrap(),
        LayoutFlavor::Bukkit {
            base: "world".to_owned()
        }
    );
    cleanup(&world);
}

#[test]
fn discovers_custom_level_name_trio() {
    // `level-name: srv` servers use srv/srv_nether/srv_the_end.
    let root = tempdir("levelname");
    let image = one_chunk_image();
    write(&root.join("srv/region/r.0.0.mca"), &image);
    write(&root.join("srv_nether/DIM-1/region/r.0.0.mca"), &image);
    write(&root.join("srv_the_end/DIM1/region/r.0.0.mca"), &image);
    let found = discover(&root).unwrap();
    assert_eq!(found.len(), 3);
    assert!(
        found
            .iter()
            .any(|r| r.dim == Dimension::OVERWORLD && r.path == root.join("srv/region/r.0.0.mca"))
    );
    assert!(
        found.iter().any(|r| r.dim == Dimension::NETHER
            && r.path == root.join("srv_nether/DIM-1/region/r.0.0.mca"))
    );
    assert_eq!(
        detect_flavor(&root).unwrap(),
        LayoutFlavor::Bukkit {
            base: "srv".to_owned()
        }
    );
    // Derivation follows the custom base.
    assert_eq!(
        derive_path(
            &root,
            &LayoutFlavor::Bukkit {
                base: "srv".to_owned()
            },
            Dimension::NETHER,
            RegionKind::REGION,
            0,
            0
        )
        .unwrap(),
        root.join("srv_nether/DIM-1/region/r.0.0.mca")
    );
    cleanup(&root);
}

#[test]
fn multiverse_worlds_never_collide() {
    // Main trio keeps vanilla codes; extra world folders hash theirs, so
    // same-environment worlds cannot silently share coordinates.
    let root = tempdir("multiverse");
    let image = one_chunk_image();
    write(&root.join("world/region/r.0.0.mca"), &image);
    write(&root.join("world_nether/DIM-1/region/r.0.0.mca"), &image);
    write(&root.join("sky/region/r.0.0.mca"), &image);
    write(&root.join("sky_nether/DIM-1/region/r.0.0.mca"), &image);
    let found = discover(&root).unwrap();
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
        derive_path(
            &root,
            &LayoutFlavor::Bukkit {
                base: "world".to_owned()
            },
            hashed[0].dim,
            RegionKind::REGION,
            0,
            0
        ),
        Err(sekai_world::WorldError::UnknownRegionPath { .. })
    ));
    cleanup(&root);
}

#[test]
fn trio_wins_over_conversion_leftovers() {
    // A stale root-level DIM-1 next to a live Bukkit trio: the trio is live
    // server data, the leftover is pre-migration residue.
    let root = tempdir("stale");
    let mut stale = one_chunk_image();
    stale[8192 + 4] = 9;
    let mut live = one_chunk_image();
    live[8192 + 4] = 7;
    write(&root.join("DIM-1/region/r.0.0.mca"), &stale);
    write(&root.join("world/region/r.1.0.mca"), &one_chunk_image());
    write(&root.join("world_nether/DIM-1/region/r.0.0.mca"), &live);
    let found = discover(&root).unwrap();
    let nether: Vec<_> = found
        .iter()
        .filter(|r| r.dim == Dimension::NETHER)
        .collect();
    assert_eq!(nether.len(), 1);
    assert_eq!(open_image(&nether[0].path).unwrap()[8192 + 4], 7);
    cleanup(&root);
}

#[test]
fn nested_vanilla_copy_gets_hashed_codes() {
    // A full vanilla world copied under the root keeps working under hashed
    // namespaces instead of colliding with the outer namespaces.
    let root = tempdir("nested");
    let image = one_chunk_image();
    write(&root.join("region/r.0.0.mca"), &image);
    write(&root.join("old/region/r.0.0.mca"), &image);
    write(&root.join("old/DIM-1/region/r.0.0.mca"), &image);
    let found = discover(&root).unwrap();
    assert_eq!(found.len(), 3);
    assert!(
        found
            .iter()
            .any(|r| r.dim == Dimension::OVERWORLD && r.path == root.join("region/r.0.0.mca"))
    );
    let hashed = found
        .iter()
        .filter(|r| r.dim != Dimension::OVERWORLD)
        .count();
    assert_eq!(hashed, 2);
    cleanup(&root);
}

#[test]
fn missing_world_is_an_error() {
    let world = tempdir("gone");
    cleanup(&world);
    assert!(matches!(
        discover(&world),
        Err(sekai_world::WorldError::Io { .. })
    ));
}

#[test]
fn derives_both_layouts() {
    let world = Path::new("/w");
    assert_eq!(
        derive_path(
            world,
            &LayoutFlavor::Legacy,
            Dimension::NETHER,
            RegionKind::REGION,
            -1,
            2
        )
        .unwrap(),
        Path::new("/w/DIM-1/region/r.-1.2.mca")
    );
    assert_eq!(
        derive_path(
            world,
            &LayoutFlavor::New,
            Dimension::END,
            RegionKind::POI,
            0,
            0
        )
        .unwrap(),
        Path::new("/w/dimensions/minecraft/the_end/poi/r.0.0.mca")
    );
    assert!(matches!(
        derive_path(
            world,
            &LayoutFlavor::New,
            Dimension::new(4242),
            RegionKind::REGION,
            0,
            0
        ),
        Err(sekai_world::WorldError::UnknownRegionPath { .. })
    ));
    assert!(matches!(
        derive_path(
            world,
            &LayoutFlavor::Legacy,
            Dimension::OVERWORLD,
            RegionKind::new(9),
            0,
            0
        ),
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
        a.header_hash,
        fingerprint_file(&path, key).unwrap().header_hash
    );
    // Missing file is an I/O error, not a fingerprint.
    cleanup(&world);
    assert!(matches!(
        fingerprint_file(&path, key),
        Err(sekai_world::WorldError::Io { .. })
    ));
}

#[test]
fn scan_counts_chunks_without_writing() {
    let world = tempdir("scan");
    write(&world.join("region/r.0.0.mca"), &one_chunk_image());
    write(&world.join("region/r.1.0.mca"), &[0u8; 8192]);
    write(&world.join("region/r.2.0.mca"), &[]);
    let mut entries = scan_world(&world).unwrap();
    entries.sort_by_key(|e| e.region_x);
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].chunks, 1);
    assert_eq!(entries[0].file_bytes % 4096, 0);
    assert_eq!(entries[0].header_hash.len(), 64);
    assert_eq!(entries[1].chunks, 0);
    assert_eq!(entries[2].chunks, 0);
    assert_eq!(entries[2].file_bytes, 0);
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
