//! Chunk AST diff operations over CAS and metadata stores.

use std::path::Path;
use std::time::{Duration, Instant};

use sekai_core::{
    BlobHash, ChunkCoord, DEFAULT_IGNORED, MetaStore, NbtDiffEntry, SnapshotId, diff_nbt,
};

use crate::error::AppError;

/// Per-phase timings for chunk AST diffs. Informational only; never
/// changes diff semantics.
#[derive(Debug, Clone, Default)]
pub struct DiffTimings {
    /// Wall-clock total.
    pub total: Duration,
    /// Snapshot metadata lookup plus CAS blob fetch, both sides.
    pub blob_fetch: Duration,
    /// Payload decompression, both sides.
    pub decompress: Duration,
    /// AST diff computation.
    pub diff_compute: Duration,
}

/// Compute AST diff between two stored chunk blob hashes in `store_url`.
pub async fn diff_blobs(
    store_url: &str,
    old_hash: &BlobHash,
    new_hash: &BlobHash,
    ignore: Option<&[&str]>,
) -> Result<Vec<NbtDiffEntry>, AppError> {
    let store = super::open_store(store_url).await?;
    let ignore_set = ignore.unwrap_or(DEFAULT_IGNORED);

    let mut old_compressed = Vec::new();
    let mut new_compressed = Vec::new();

    store.cas().fetch_blob(old_hash, &mut old_compressed)?;
    store.cas().fetch_blob(new_hash, &mut new_compressed)?;

    let old_nbt = decompress_chunk(&old_compressed)?;
    let new_nbt = decompress_chunk(&new_compressed)?;

    Ok(diff_nbt(&old_nbt, &new_nbt, ignore_set)?)
}

/// Fetch raw (still compressed) chunk payload directly from a world directory.
fn read_world_chunk_compressed(world: &Path, coord: &ChunkCoord) -> Result<Vec<u8>, AppError> {
    let rx = coord.region_x();
    let rz = coord.region_z();

    let regions = sekai_world::discover(world)?;
    let region_ref = regions
        .into_iter()
        .find(|r| {
            r.dim == coord.dim && r.kind == coord.kind && r.region_x == rx && r.region_z == rz
        })
        .ok_or(AppError::ChunkNotFoundInWorld { coord: *coord })?;

    let bytes = sekai_world::open_image(&region_ref.path)?;
    let failed = |source| AppError::RegionFailed {
        path: region_ref.path.clone(),
        source,
    };
    let image = sekai_anvil::RegionImage::from_bytes(bytes, rx, rz).map_err(&failed)?;

    let payload = image.chunk_payload(coord.x, coord.z).map_err(&failed)?;
    let compressed = payload.ok_or(AppError::ChunkNotFoundInWorld { coord: *coord })?;
    Ok(compressed.to_vec())
}

/// Decompress one raw chunk payload into NBT bytes.
fn decompress_chunk(compressed: &[u8]) -> Result<Vec<u8>, AppError> {
    let mut nbt = Vec::new();
    sekai_anvil::decompress_into(compressed, &mut nbt)?;
    Ok(nbt)
}

/// Compute AST diff for a chunk coordinate between two snapshots in `store_url`.
pub async fn diff_chunk(
    store_url: &str,
    old_snapshot: SnapshotId,
    new_snapshot: SnapshotId,
    coord: &ChunkCoord,
    ignore: Option<&[&str]>,
) -> Result<Vec<NbtDiffEntry>, AppError> {
    let (diffs, _) = diff_chunks(store_url, old_snapshot, new_snapshot, &[*coord], ignore).await?;
    Ok(diffs
        .into_iter()
        .next()
        .map_or_else(Vec::new, |diff| diff.entries))
}

/// One chunk's AST diff entries between two sources.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkDiff {
    /// Chunk the entries belong to.
    pub coord: ChunkCoord,
    /// AST differences; empty when both sides decode identically.
    pub entries: Vec<NbtDiffEntry>,
}

/// Compute AST diffs for explicit chunk coordinates.
///
/// Compares two snapshots in `store_url`, additionally returning
/// per-phase timings. Coordinates unknown to either snapshot resolve
/// through fallback; tombstoned sides error like the single-chunk API.
pub async fn diff_chunks(
    store_url: &str,
    old_snapshot: SnapshotId,
    new_snapshot: SnapshotId,
    coords: &[ChunkCoord],
    ignore: Option<&[&str]>,
) -> Result<(Vec<ChunkDiff>, DiffTimings), AppError> {
    let total_started = Instant::now();
    let store = super::open_store(store_url).await?;
    let ignore_set = ignore.unwrap_or(DEFAULT_IGNORED);
    let mut timings = DiffTimings::default();
    let mut out = Vec::with_capacity(coords.len());
    for coord in coords {
        let started = Instant::now();
        let old_compressed = snapshot_chunk_compressed(&store, old_snapshot, coord).await?;
        let new_compressed = snapshot_chunk_compressed(&store, new_snapshot, coord).await?;
        timings.blob_fetch += started.elapsed();
        let started = Instant::now();
        let old_nbt = decompress_chunk(&old_compressed)?;
        let new_nbt = decompress_chunk(&new_compressed)?;
        timings.decompress += started.elapsed();
        let started = Instant::now();
        let entries = diff_nbt(&old_nbt, &new_nbt, ignore_set)?;
        timings.diff_compute += started.elapsed();
        out.push(ChunkDiff {
            coord: *coord,
            entries,
        });
    }
    timings.total = total_started.elapsed();
    Ok((out, timings))
}

/// Effective chunk coordinates of one snapshot (fallback-resolved set).
pub async fn snapshot_chunk_coords(
    store_url: &str,
    snapshot: SnapshotId,
) -> Result<Vec<ChunkCoord>, AppError> {
    let store = super::open_store(store_url).await?;
    let mut coords = Vec::new();
    store
        .meta()
        .visit_snapshot_chunks(snapshot, |entry| {
            coords.push(entry.coord);
            true
        })
        .await?;
    Ok(coords)
}

/// Chunk coordinates present on disk in `world`.
pub fn world_chunk_coords(world: &Path) -> Result<Vec<ChunkCoord>, AppError> {
    let mut coords = Vec::new();
    for region in sekai_world::discover(world)? {
        let bytes = sekai_world::open_image(&region.path)?;
        let failed = |source| AppError::RegionFailed {
            path: region.path.clone(),
            source,
        };
        let image = sekai_anvil::RegionImage::from_bytes(bytes, region.region_x, region.region_z)
            .map_err(&failed)?;
        image
            .visit_chunks(|chunk| {
                coords.push(ChunkCoord::new(region.dim, region.kind, chunk.x, chunk.z));
                true
            })
            .map_err(&failed)?;
    }
    coords.sort();
    coords.dedup();
    Ok(coords)
}

/// Fetch raw (still compressed) chunk payload for a snapshot from an open store.
async fn snapshot_chunk_compressed(
    store: &sekai_storage::SqliteStore,
    snapshot_id: SnapshotId,
    coord: &ChunkCoord,
) -> Result<Vec<u8>, AppError> {
    let missing = || AppError::ChunkNotFoundInSnapshot {
        snapshot_id,
        coord: *coord,
    };
    let entry = store
        .meta()
        .lookup_chunk(snapshot_id, coord)
        .await?
        .ok_or_else(missing)?;
    let hash = entry.blob.ok_or_else(missing)?;

    let mut compressed = Vec::new();
    store.cas().fetch_blob(&hash, &mut compressed)?;
    Ok(compressed)
}

/// Compute AST diff for a chunk coordinate between current world state and a snapshot in `store_url`.
///
/// If `snapshot` is `None`, the latest snapshot in the store is used.
pub async fn diff_world_chunk(
    world: &Path,
    store_url: &str,
    snapshot: Option<SnapshotId>,
    coord: &ChunkCoord,
    ignore: Option<&[&str]>,
) -> Result<Vec<NbtDiffEntry>, AppError> {
    let (diffs, _) = diff_world_chunks(world, store_url, snapshot, &[*coord], ignore).await?;
    Ok(diffs
        .into_iter()
        .next()
        .map_or_else(Vec::new, |diff| diff.entries))
}

/// Compute AST diffs for explicit chunk coordinates between current world
/// state and a snapshot in `store_url`, additionally returning per-phase
/// timings.
///
/// If `snapshot` is `None`, the latest snapshot in the store is used.
/// World-side acquisition (region lookup, read, parse) counts under
/// `blob_fetch`; both sides' decompression under `decompress`.
pub async fn diff_world_chunks(
    world: &Path,
    store_url: &str,
    snapshot: Option<SnapshotId>,
    coords: &[ChunkCoord],
    ignore: Option<&[&str]>,
) -> Result<(Vec<ChunkDiff>, DiffTimings), AppError> {
    let total_started = Instant::now();
    let snapshot_id = if let Some(id) = snapshot {
        id
    } else {
        super::latest_snapshot_id(store_url).await?
    };

    let store = super::open_store(store_url).await?;
    let ignore_set = ignore.unwrap_or(DEFAULT_IGNORED);
    let mut timings = DiffTimings::default();
    let mut out = Vec::with_capacity(coords.len());
    for coord in coords {
        let started = Instant::now();
        let snapshot_compressed = snapshot_chunk_compressed(&store, snapshot_id, coord).await?;
        let world_compressed = read_world_chunk_compressed(world, coord)?;
        timings.blob_fetch += started.elapsed();
        let started = Instant::now();
        let snapshot_nbt = decompress_chunk(&snapshot_compressed)?;
        let world_nbt = decompress_chunk(&world_compressed)?;
        timings.decompress += started.elapsed();
        let started = Instant::now();
        let entries = diff_nbt(&snapshot_nbt, &world_nbt, ignore_set)?;
        timings.diff_compute += started.elapsed();
        out.push(ChunkDiff {
            coord: *coord,
            entries,
        });
    }
    timings.total = total_started.elapsed();
    Ok((out, timings))
}
