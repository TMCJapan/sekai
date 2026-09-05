//! Filesystem round-trips: writer commit <-> reader open.
//!
//! Rationale: unit tests pin the in-memory layout; these tests pin the
//! on-disk contract - atomic swap leaves no temp files behind, committed
//! bytes read back byte-identical, and removals land as absent sectors.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sekai_core::{ChunkCoord, Dimension, RegionKind, RegionReader as _, RegionWriter as _};
use sekai_mca::{McaError, RegionFile, RegionFileWriter};

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Isolated scratch directory (removed on drop, best effort).
struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "sekai-mca-test-{}-{}",
            std::process::id(),
            DIR_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn region(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    /// Non-`.mca` leftovers (temp files) still lying around, if any.
    fn leftovers(&self) -> Vec<PathBuf> {
        fs::read_dir(&self.path)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_none_or(|e| e != "mca"))
            .collect()
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

const fn coord(x: i32, z: i32) -> ChunkCoord {
    ChunkCoord::new(Dimension::OVERWORLD, RegionKind::REGION, x, z)
}

fn collect(region: &RegionFile) -> Vec<(ChunkCoord, Vec<u8>)> {
    let mut out = Vec::new();
    region
        .visit_chunks(|c| {
            out.push((c.coord, c.payload.to_vec()));
            true
        })
        .unwrap();
    out.sort();
    out
}

#[test]
fn commit_then_open_round_trip() {
    let dir = ScratchDir::new();
    let target = dir.region("r.0.0.mca");
    let payload_a = vec![2u8; 100];
    let payload_b: Vec<u8> = (0..5000u32)
        .map(|i| u8::try_from(i % 251).unwrap_or_default())
        .collect();

    let mut w = RegionFileWriter::create(&target, Dimension::OVERWORLD, RegionKind::REGION, 0xABCD)
        .unwrap();
    w.stage_chunk(&coord(0, 0), &payload_a).unwrap();
    w.stage_chunk(&coord(31, 31), &payload_b).unwrap();
    // Staged then removed: lands as an absent sector (tombstone).
    w.stage_chunk(&coord(1, 1), &[3, 1]).unwrap();
    w.stage_remove(&coord(1, 1)).unwrap();
    w.commit().unwrap();

    assert!(dir.leftovers().is_empty());
    let raw = fs::read(&target).unwrap();
    assert!(raw.len().is_multiple_of(4096));

    let region = RegionFile::open(&target, Dimension::OVERWORLD, RegionKind::REGION).unwrap();
    assert_eq!(
        collect(&region),
        vec![(coord(0, 0), payload_a), (coord(31, 31), payload_b)]
    );
    // Timestamps: present slots carry the commit stamp.
    assert_eq!(&raw[4096..4100], &0xABCDu32.to_be_bytes());
    assert_eq!(
        &raw[4096 + 1023 * 4..4096 + 1024 * 4],
        &0xABCDu32.to_be_bytes()
    );
}

#[test]
fn commit_replaces_existing_target_atomically() {
    let dir = ScratchDir::new();
    let target = dir.region("r.-2.3.mca");
    fs::write(&target, vec![9u8; 8192]).unwrap();

    // Region r.-2.3 spans chunks x in [-64, -33], z in [96, 127].
    let mut w =
        RegionFileWriter::create(&target, Dimension::OVERWORLD, RegionKind::REGION, 7).unwrap();
    let payload = vec![3u8; 10];
    w.stage_chunk(
        &ChunkCoord::new(Dimension::OVERWORLD, RegionKind::REGION, -64, 127),
        &payload,
    )
    .unwrap();
    w.commit().unwrap();

    assert!(dir.leftovers().is_empty());
    let region = RegionFile::open(&target, Dimension::OVERWORLD, RegionKind::REGION).unwrap();
    assert_eq!(region.region_x(), -2);
    assert_eq!(region.region_z(), 3);
    assert_eq!(collect(&region).len(), 1);
}

#[test]
fn open_reads_names_from_disk() {
    let dir = ScratchDir::new();
    // Valid image under a non-region name: open must reject the name,
    // proving coordinates come from the file name, not content.
    let odd = dir.region("notes.mca");
    fs::write(&odd, vec![0u8; 8192]).unwrap();
    assert!(matches!(
        RegionFile::open(&odd, Dimension::OVERWORLD, RegionKind::REGION),
        Err(McaError::BadFilename { .. })
    ));
    // Missing file surfaces path-carrying I/O errors.
    let missing = dir.region("r.9.9.mca");
    let err = RegionFile::open(&missing, Dimension::OVERWORLD, RegionKind::REGION).unwrap_err();
    assert!(matches!(err, McaError::Io { .. }));
    assert!(format!("{err}").contains("r.9.9.mca"));
}

#[test]
fn second_commit_drops_unstaged_chunks() {
    let dir = ScratchDir::new();
    let target = dir.region("r.0.0.mca");
    let stamp = |w: &mut RegionFileWriter, x: i32, data: &[u8]| {
        w.stage_chunk(&coord(x, 0), data).unwrap();
    };

    let mut w =
        RegionFileWriter::create(&target, Dimension::OVERWORLD, RegionKind::REGION, 1).unwrap();
    stamp(&mut w, 0, &[2, 1]);
    stamp(&mut w, 1, &[2, 2]);
    w.commit().unwrap();

    // Rollback-style rewrite carrying only chunk 0: chunk 1 must vanish.
    let mut w =
        RegionFileWriter::create(&target, Dimension::OVERWORLD, RegionKind::REGION, 2).unwrap();
    stamp(&mut w, 0, &[2, 1]);
    w.commit().unwrap();

    let region = RegionFile::open(&target, Dimension::OVERWORLD, RegionKind::REGION).unwrap();
    assert_eq!(collect(&region).len(), 1);
    assert!(Path::new(&target).exists());
}
