//! Chunk AST diff operations over CAS and metadata stores.

use std::path::Path;

use sekai_core::{
    BlobHash, ChunkCoord, DEFAULT_IGNORED, MetaStore, NbtDiffEntry, SnapshotId, diff_nbt,
};

use crate::error::AppError;

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

    let mut old_nbt = Vec::new();
    let mut new_nbt = Vec::new();

    sekai_anvil::decompress_into(&old_compressed, &mut old_nbt)?;
    sekai_anvil::decompress_into(&new_compressed, &mut new_nbt)?;

    let diffs = diff_nbt(&old_nbt, &new_nbt, ignore_set)?;
    Ok(diffs)
}

/// Fetch and decompress raw chunk NBT directly from a world directory.
pub fn read_world_chunk_nbt(world: &Path, coord: &ChunkCoord) -> Result<Vec<u8>, AppError> {
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
    let diffs = diff_chunks(store_url, old_snapshot, new_snapshot, &[*coord], ignore).await?;
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
/// Compares two snapshots in `store_url`. Coordinates unknown to either
/// snapshot resolve through fallback; tombstoned sides error like the
/// single-chunk API.
pub async fn diff_chunks(
    store_url: &str,
    old_snapshot: SnapshotId,
    new_snapshot: SnapshotId,
    coords: &[ChunkCoord],
    ignore: Option<&[&str]>,
) -> Result<Vec<ChunkDiff>, AppError> {
    let store = super::open_store(store_url).await?;
    let ignore_set = ignore.unwrap_or(DEFAULT_IGNORED);
    let mut out = Vec::with_capacity(coords.len());
    for coord in coords {
        let old_nbt = snapshot_chunk_nbt(&store, old_snapshot, coord).await?;
        let new_nbt = snapshot_chunk_nbt(&store, new_snapshot, coord).await?;
        let entries = diff_nbt(&old_nbt, &new_nbt, ignore_set)?;
        out.push(ChunkDiff {
            coord: *coord,
            entries,
        });
    }
    Ok(out)
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

/// Fetch and decompress raw chunk NBT for a snapshot from an open store.
async fn snapshot_chunk_nbt(
    store: &sekai_storage::SqliteStore,
    snapshot_id: SnapshotId,
    coord: &ChunkCoord,
) -> Result<Vec<u8>, AppError> {
    let entry = store.meta().lookup_chunk(snapshot_id, coord).await?.ok_or(
        AppError::ChunkNotFoundInSnapshot {
            snapshot_id,
            coord: *coord,
        },
    )?;

    let hash = entry.blob.ok_or(AppError::ChunkNotFoundInSnapshot {
        snapshot_id,
        coord: *coord,
    })?;

    let mut compressed = Vec::new();
    store.cas().fetch_blob(&hash, &mut compressed)?;

    let mut nbt = Vec::new();
    sekai_anvil::decompress_into(&compressed, &mut nbt)?;
    Ok(nbt)
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
    let diffs = diff_world_chunks(world, store_url, snapshot, &[*coord], ignore).await?;
    Ok(diffs
        .into_iter()
        .next()
        .map_or_else(Vec::new, |diff| diff.entries))
}

/// Compute AST diffs for explicit chunk coordinates between current world
/// state and a snapshot in `store_url`.
///
/// If `snapshot` is `None`, the latest snapshot in the store is used.
pub async fn diff_world_chunks(
    world: &Path,
    store_url: &str,
    snapshot: Option<SnapshotId>,
    coords: &[ChunkCoord],
    ignore: Option<&[&str]>,
) -> Result<Vec<ChunkDiff>, AppError> {
    let snapshot_id = match snapshot {
        Some(id) => id,
        None => super::latest_snapshot_id(store_url).await?,
    };

    let store = super::open_store(store_url).await?;
    let ignore_set = ignore.unwrap_or(DEFAULT_IGNORED);
    let mut out = Vec::with_capacity(coords.len());
    for coord in coords {
        let snapshot_nbt = snapshot_chunk_nbt(&store, snapshot_id, coord).await?;
        let world_nbt = read_world_chunk_nbt(world, coord)?;
        let entries = diff_nbt(&snapshot_nbt, &world_nbt, ignore_set)?;
        out.push(ChunkDiff {
            coord: *coord,
            entries,
        });
    }
    Ok(out)
}
