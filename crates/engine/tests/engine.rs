//! MVP end-to-end: backup, change, backup, rollback.
//!
//! Rationale: these tests pin the whole promise - two backups deduplicate
//! and tombstone correctly, rollback restores byte-identical payloads,
//! post-snapshot files disappear under strict rollback, and every world
//! generation (legacy / new / custom) is discovered with a stable
//! namespace.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sekai_core::{
    BlobHash, BlobStore as _, ChunkCoord, Dimension, MetaStore as _, RegionKind, RegionReader as _,
};
use sekai_engine::{EngineError, Store, backup, discover, gc_apply, gc_plan, rollback};

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "sekai-engine-test-{}-{}",
            std::process::id(),
            DIR_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn world(&self) -> PathBuf {
        self.root.join("world")
    }

    fn store(&self) -> PathBuf {
        self.root.join("store")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

type WorldState = BTreeMap<(Dimension, RegionKind, i32, i32), Vec<u8>>;

/// Write one region file with exact payloads.
fn write_region(path: &Path, dim: Dimension, kind: RegionKind, chunks: &[(i32, i32, Vec<u8>)]) {
    use sekai_core::RegionWriter as _;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut w = sekai_mca::RegionFileWriter::create(path, dim, kind, 0x5EED).unwrap();
    for (x, z, payload) in chunks {
        w.stage_chunk(&ChunkCoord::new(dim, kind, *x, *z), payload)
            .unwrap();
    }
    w.commit().unwrap();
}

/// Read every chunk payload of a world.
fn read_world(world: &Path) -> WorldState {
    let mut out = WorldState::new();
    for r in discover(world).unwrap() {
        let file = sekai_mca::RegionFile::open(&r.path, r.dim, r.kind).unwrap();
        file.visit_chunks(|c| {
            out.insert(
                (c.coord.dim, c.coord.kind, c.coord.x, c.coord.z),
                c.payload.to_vec(),
            );
            true
        })
        .unwrap();
    }
    out
}

const OVER: Dimension = Dimension::OVERWORLD;
const REGION: RegionKind = RegionKind::REGION;

#[test]
fn backup_twice_then_rollback_each() {
    let scratch = Scratch::new();
    let world = scratch.world();
    let region = world.join("region").join("r.0.0.mca");

    write_region(
        &region,
        OVER,
        REGION,
        &[(0, 0, vec![2, 1, 2, 3]), (1, 0, vec![2, 4, 5])],
    );
    let mut store = Store::open(&scratch.store()).unwrap();

    let r1 = backup(&world, &mut store).unwrap();
    assert_eq!(r1.chunks, 2);
    assert_eq!(r1.new_blobs, 2);
    assert_eq!(r1.tombstones, 0);

    // Change one chunk, delete one, add one.
    write_region(
        &region,
        OVER,
        REGION,
        &[(0, 0, vec![2, 9, 9, 9]), (2, 0, vec![3, 7])],
    );
    let r2 = backup(&world, &mut store).unwrap();
    assert_eq!(r2.chunks, 2);
    assert_eq!(r2.new_blobs, 2);
    assert_eq!(r2.tombstones, 1);

    // The deleted chunk is a tombstone in the second snapshot.
    let tomb = store
        .meta()
        .lookup_chunk(r2.snapshot, &ChunkCoord::new(OVER, REGION, 1, 0))
        .unwrap()
        .unwrap();
    assert!(tomb.is_tombstone());

    // Rollback to the first snapshot: byte-identical restoration.
    let rb1 = rollback(&world, &mut store, r1.snapshot).unwrap();
    assert_eq!(rb1.chunks_restored, 2);
    assert_eq!(
        read_world(&world),
        WorldState::from([
            ((OVER, REGION, 0, 0), vec![2, 1, 2, 3]),
            ((OVER, REGION, 1, 0), vec![2, 4, 5]),
        ])
    );

    // And forward again to the second.
    let rb2 = rollback(&world, &mut store, r2.snapshot).unwrap();
    assert_eq!(rb2.chunks_restored, 2);
    assert_eq!(
        read_world(&world),
        WorldState::from([
            ((OVER, REGION, 0, 0), vec![2, 9, 9, 9]),
            ((OVER, REGION, 2, 0), vec![3, 7]),
        ])
    );
}

#[test]
fn rollback_deletes_post_snapshot_files() {
    let scratch = Scratch::new();
    let world = scratch.world();
    write_region(
        &world.join("region").join("r.0.0.mca"),
        OVER,
        REGION,
        &[(0, 0, vec![2, 1])],
    );
    let mut store = Store::open(&scratch.store()).unwrap();
    let r1 = backup(&world, &mut store).unwrap();

    // Region created after the snapshot must not survive rollback.
    let extra = world.join("region").join("r.1.0.mca");
    write_region(&extra, OVER, REGION, &[(32, 0, vec![2, 2])]);

    let report = rollback(&world, &mut store, r1.snapshot).unwrap();
    assert_eq!(report.files_deleted, 1);
    assert!(!extra.exists());
    assert_eq!(read_world(&world).len(), 1);
}

#[test]
fn rollback_rejects_unknown_snapshot() {
    let scratch = Scratch::new();
    let world = scratch.world();
    write_region(
        &world.join("region").join("r.0.0.mca"),
        OVER,
        REGION,
        &[(0, 0, vec![2, 1])],
    );
    let mut store = Store::open(&scratch.store()).unwrap();
    backup(&world, &mut store).unwrap();

    let err = rollback(&world, &mut store, sekai_core::SnapshotId(999)).unwrap_err();
    assert!(matches!(err, EngineError::UnknownSnapshot { id: 999 }));
}

#[test]
fn discovers_all_layout_generations() {
    let scratch = Scratch::new();
    let world = scratch.world();
    // Legacy nether.
    write_region(
        &world.join("DIM-1").join("region").join("r.0.0.mca"),
        Dimension::NETHER,
        REGION,
        &[(0, 0, vec![2, 1])],
    );
    // New-layout end entities.
    write_region(
        &world
            .join("dimensions")
            .join("minecraft")
            .join("the_end")
            .join("entities")
            .join("r.0.0.mca"),
        Dimension::END,
        RegionKind::ENTITIES,
        &[(0, 0, vec![2, 2])],
    );
    // Modded dimension.
    write_region(
        &world
            .join("dimensions")
            .join("aether")
            .join("sky")
            .join("region")
            .join("r.0.0.mca"),
        Dimension::OVERWORLD, // namespace comes from discovery, not this hint
        REGION,
        &[(0, 0, vec![2, 3])],
    );

    let found = discover(&world).unwrap();
    assert_eq!(found.len(), 3);
    let dims: Vec<Dimension> = {
        let mut v: Vec<Dimension> = found.iter().map(|r| r.dim).collect();
        v.sort_by_key(|d| d.raw());
        v
    };
    assert!(dims.contains(&Dimension::NETHER));
    assert!(dims.contains(&Dimension::END));
    // Custom dimension keeps a non-vanilla, stable code across runs.
    let custom = dims
        .iter()
        .find(|d| ![Dimension::OVERWORLD, Dimension::NETHER, Dimension::END].contains(d))
        .unwrap();
    assert_eq!(
        *custom,
        discover(&world)
            .unwrap()
            .iter()
            .find(|r| r.dim == *custom)
            .unwrap()
            .dim
    );

    let mut store = Store::open(&scratch.store()).unwrap();
    let report = backup(&world, &mut store).unwrap();
    assert_eq!(report.chunks, 3);
}

#[test]
fn backup_rejects_missing_world() {
    let scratch = Scratch::new();
    let mut store = Store::open(&scratch.store()).unwrap();
    let err = backup(&scratch.root.join("nope"), &mut store).unwrap_err();
    assert!(matches!(err, EngineError::Io { .. }));
}

#[test]
fn backup_tolerates_zero_length_region() {
    let scratch = Scratch::new();
    let world = scratch.world();
    write_region(
        &world.join("region").join("r.0.0.mca"),
        OVER,
        REGION,
        &[(0, 0, vec![2, 1])],
    );
    // Not-yet-generated region placeholder: servers can leave `0`-byte files.
    let empty_path = world.join("region").join("r.1.0.mca");
    fs::create_dir_all(empty_path.parent().unwrap()).unwrap();
    fs::write(&empty_path, Vec::new()).unwrap();

    let mut store = Store::open(&scratch.store()).unwrap();
    let report = backup(&world, &mut store).unwrap();
    assert_eq!(report.chunks, 1);
    assert_eq!(read_world(&world).len(), 1);

    // The placeholder stays on disk and keeps backing up cleanly.
    assert!(empty_path.exists());
    let second = backup(&world, &mut store).unwrap();
    assert_eq!(second.chunks, 1);
}

#[test]
fn gc_reclaims_only_orphans() {
    let scratch = Scratch::new();
    let world = scratch.world();
    write_region(
        &world.join("region").join("r.0.0.mca"),
        OVER,
        REGION,
        &[(0, 0, vec![2, 1])],
    );
    let mut store = Store::open(&scratch.store()).unwrap();
    backup(&world, &mut store).unwrap();

    // Torn-write leftover: a blob with no referencing history row.
    let orphan = BlobHash([0xEE; 32]);
    assert!(store.cas_mut().put(&orphan, b"orphan").unwrap());

    // Plan is read-only and names exactly the orphan.
    let plan = gc_plan(&store).unwrap();
    assert_eq!(plan.orphans(), &[orphan]);
    assert_eq!(plan.examined(), 2);
    assert!(store.cas().contains(&orphan).unwrap());

    let report = gc_apply(&mut store, &plan).unwrap();
    assert_eq!(report.candidates, 1);
    assert_eq!(report.orphans, 1);
    assert_eq!(report.removed, 1);
    assert!(!store.cas().contains(&orphan).unwrap());

    // The referenced blob survives; a second cycle finds nothing.
    let latest = store.meta().latest_snapshot().unwrap().unwrap();
    let row = store
        .meta()
        .lookup_chunk(latest.id, &ChunkCoord::new(OVER, REGION, 0, 0))
        .unwrap()
        .unwrap();
    assert!(store.cas().contains(&row.blob.unwrap()).unwrap());
    assert!(gc_plan(&store).unwrap().is_empty());
}
