//! Chunk AST diff operations over CAS and metadata stores.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use sekai_core::{
    BlobHash, BlobStore as _, ChunkCoord, CommitId, DEFAULT_IGNORED, MetaStore, NbtDiffEntry,
    RegionKey, diff_nbt,
};
use sekai_storage::{BlockingBlobStore, SekaiStore, StoreLockMode};
use sekai_world::{RegionFile, WorldLockMode, WorldTree};

use crate::error::AppError;
use crate::instance::{OperationLocks, SekaiInstance, WorldHandle, WorldHandleMut};
use crate::regions::discover_regions;

/// Per-phase timings for chunk AST diffs. Informational only; never
/// changes diff semantics.
#[derive(Debug, Clone, Default)]
pub struct DiffTimings {
    /// Wall-clock total.
    pub total: Duration,
    /// Commit metadata lookup plus CAS blob fetch, both sides.
    pub blob_fetch: Duration,
    /// Payload decompression, both sides.
    pub decompress: Duration,
    /// AST diff computation.
    pub diff_compute: Duration,
}

impl<S: SekaiStore> SekaiInstance<S> {
    /// Compute AST diff between two stored chunk blob hashes.
    pub async fn diff_blobs(
        &self,
        old_hash: &BlobHash,
        new_hash: &BlobHash,
        ignore: Option<&[&str]>,
    ) -> Result<Vec<NbtDiffEntry>, AppError> {
        let _lock = self.lock_store(StoreLockMode::Shared)?;
        let ignore_set = ignore.unwrap_or(DEFAULT_IGNORED);

        let mut old_compressed = Vec::new();
        let mut new_compressed = Vec::new();

        self.store
            .cas()
            .fetch_into(old_hash, &mut old_compressed)
            .await?;
        self.store
            .cas()
            .fetch_into(new_hash, &mut new_compressed)
            .await?;

        let old_nbt = decompress_chunk(&old_compressed)?;
        let new_nbt = decompress_chunk(&new_compressed)?;

        Ok(diff_nbt(&old_nbt, &new_nbt, ignore_set)?)
    }
}

/// Fetch raw (still compressed) chunk payload directly from a world directory.
/// Returns `None` when the region file or chunk entry is absent; corrupt
/// files still fail loudly.
fn read_world_chunk_compressed<R: RegionFile>(
    regions: &BTreeMap<RegionKey, R>,
    coord: &ChunkCoord,
) -> Result<Option<Vec<u8>>, AppError>
where
    AppError: From<R::Error>,
{
    let key = RegionKey::of(*coord);
    let Some(region) = regions.get(&key) else {
        return Ok(None);
    };

    let bytes = region.read()?;
    let failed = |source| AppError::RegionFailed {
        location: region.to_string(),
        source,
    };
    let image = sekai_anvil::RegionImage::from_bytes(bytes, key.rx, key.rz).map_err(failed)?;

    let payload = image.chunk_payload(coord.x, coord.z).map_err(failed)?;
    Ok(payload.map(<[u8]>::to_vec))
}

/// Decompress one raw chunk payload into NBT bytes.
fn decompress_chunk(compressed: &[u8]) -> Result<Vec<u8>, AppError> {
    let mut nbt = Vec::new();
    sekai_anvil::decompress_into(compressed, &mut nbt)?;
    Ok(nbt)
}

/// Per-chunk progress report for the `progress` callback.
#[derive(Debug, Clone, Copy)]
pub struct DiffProgress {
    /// Chunks diffed so far.
    pub chunks_done: usize,
    /// Chunks to diff in total.
    pub chunks_total: usize,
}

impl<S: SekaiStore> SekaiInstance<S> {
    /// Compute AST diff for a chunk coordinate between two commits.
    pub async fn diff_chunk(
        &self,
        old_commit: CommitId,
        new_commit: CommitId,
        coord: &ChunkCoord,
        ignore: Option<&[&str]>,
    ) -> Result<Vec<NbtDiffEntry>, AppError> {
        let (diffs, _) = self
            .diff_chunks(old_commit, new_commit, &[*coord], ignore, |_| {})
            .await?;
        Ok(diffs
            .into_iter()
            .next()
            .map_or_else(Vec::new, |diff| diff.entries))
    }
}

/// One chunk's AST diff entries between two sources.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkDiff {
    /// Chunk the entries belong to.
    pub coord: ChunkCoord,
    /// AST differences; empty when both sides decode identically.
    pub entries: Vec<NbtDiffEntry>,
}

impl<S: SekaiStore> SekaiInstance<S> {
    /// Compute AST diffs for explicit chunk coordinates.
    ///
    /// Compares two commits in the store, additionally returning
    /// per-phase timings. A chunk absent (or tombstoned) on a side diffs as an
    /// empty compound there: absent on both sides yields no entries, present on
    /// one side reports whole-value Added/Removed entries.
    pub async fn diff_chunks(
        &self,
        old_commit: CommitId,
        new_commit: CommitId,
        coords: &[ChunkCoord],
        ignore: Option<&[&str]>,
        progress: impl FnMut(DiffProgress) + Send,
    ) -> Result<(Vec<ChunkDiff>, DiffTimings), AppError> {
        let _lock = self.lock_store(StoreLockMode::Shared)?;
        let total_started = Instant::now();
        let ignore_set = ignore.unwrap_or(DEFAULT_IGNORED);
        let mut timings = DiffTimings::default();
        let out = diff_coords(
            coords,
            ignore_set,
            CommitDiff {
                store: &self.store,
                id: old_commit,
            },
            CommitDiff {
                store: &self.store,
                id: new_commit,
            },
            progress,
            &mut timings,
        )
        .await?;
        timings.total = total_started.elapsed();
        Ok((out, timings))
    }

    /// Effective chunk coordinates of one commit (fallback-resolved set).
    ///
    /// Takes its own shared store lock, so coordinates handed to a later
    /// [`Self::diff_chunks`] go stale if a prune runs in between (ADR-0014).
    pub async fn commit_chunk_coords(&self, commit: CommitId) -> Result<Vec<ChunkCoord>, AppError> {
        let _lock = self.lock_store(StoreLockMode::Shared)?;
        let mut coords = Vec::new();
        self.store
            .meta()
            .visit_commit_chunks(commit, |entry| {
                coords.push(entry.coord);
                true
            })
            .await?;
        Ok(coords)
    }
}

/// Chunk coordinates present on disk in `world`.
///
/// Takes its own shared world lock, so coordinates handed to a later
/// [`WorldHandle::diff_world_chunks`] go stale if a checkout runs in
/// between (ADR-0014).
pub fn world_chunk_coords<T: WorldTree>(world: &T) -> Result<Vec<ChunkCoord>, AppError>
where
    AppError: From<T::Error>,
{
    let _lock = world.lock(WorldLockMode::Shared)?;
    let mut coords = Vec::new();
    for (key, region) in discover_regions(world)? {
        let bytes = region.read()?;
        let failed = |source| AppError::RegionFailed {
            location: region.to_string(),
            source,
        };
        let image = sekai_anvil::RegionImage::from_bytes(bytes, key.rx, key.rz).map_err(failed)?;
        image
            .visit_chunks(|chunk| {
                coords.push(ChunkCoord::new(key.dim, key.kind, chunk.x, chunk.z));
                true
            })
            .map_err(failed)?;
    }
    coords.sort();
    coords.dedup();
    Ok(coords)
}

/// Fetch raw (still compressed) chunk payload for a commit from an open store.
/// Returns `None` when the chunk is unknown there or tombstoned; a present
/// blob missing from CAS still fails as corruption.
async fn commit_chunk_compressed<S: SekaiStore>(
    store: &S,
    commit_id: CommitId,
    coord: &ChunkCoord,
) -> Result<Option<Vec<u8>>, AppError> {
    let Some(entry) = store.meta().lookup_chunk(commit_id, coord).await? else {
        return Ok(None);
    };
    let Some(hash) = entry.blob else {
        return Ok(None);
    };

    let mut compressed = Vec::new();
    store.cas().fetch_blob(&hash, &mut compressed)?;
    Ok(Some(compressed))
}

/// Empty root compound: the NBT a missing chunk diffs as. Absent on both
/// sides yields no entries; present on one side reports whole-value
/// Added/Removed entries for every leaf.
const EMPTY_COMPOUND: [u8; 4] = [10, 0, 0, 0];

fn decompress_or_empty(compressed: Option<Vec<u8>>) -> Result<Vec<u8>, AppError> {
    compressed.map_or_else(|| Ok(EMPTY_COMPOUND.to_vec()), |c| decompress_chunk(&c))
}

impl<W: WorldTree, S: SekaiStore> WorldHandle<'_, W, S>
where
    AppError: From<W::Error>,
{
    /// Compute AST diff for a chunk coordinate between the bound world state
    /// and a commit.
    ///
    /// If `commit` is `None`, the latest commit in the store is used.
    pub async fn diff_world_chunk(
        &self,
        commit: Option<CommitId>,
        coord: &ChunkCoord,
        ignore: Option<&[&str]>,
    ) -> Result<Vec<NbtDiffEntry>, AppError> {
        let (diffs, _) = self
            .diff_world_chunks(commit, &[*coord], ignore, |_| {})
            .await?;
        Ok(diffs
            .into_iter()
            .next()
            .map_or_else(Vec::new, |diff| diff.entries))
    }

    /// Compute AST diffs for explicit chunk coordinates between the bound
    /// world state and a commit, additionally returning per-phase timings.
    ///
    /// If `commit` is `None`, the latest commit in the store is used.
    /// World-side acquisition (region lookup, read, parse) counts under
    /// `blob_fetch`; both sides' decompression under `decompress`.
    pub async fn diff_world_chunks(
        &self,
        commit: Option<CommitId>,
        coords: &[ChunkCoord],
        ignore: Option<&[&str]>,
        progress: impl FnMut(DiffProgress) + Send,
    ) -> Result<(Vec<ChunkDiff>, DiffTimings), AppError> {
        let locks = OperationLocks::acquire(
            self.instance,
            StoreLockMode::Shared,
            self.world,
            WorldLockMode::Shared,
        )?;
        let total_started = Instant::now();
        let commit_id = match commit {
            Some(id) => id,
            None => self.instance.latest_commit_id_locked(locks.store()).await?,
        };

        let ignore_set = ignore.unwrap_or(DEFAULT_IGNORED);
        let mut timings = DiffTimings::default();
        let out = diff_coords(
            coords,
            ignore_set,
            CommitDiff {
                store: &self.instance.store,
                id: commit_id,
            },
            WorldDiff {
                regions: discover_regions(self.world)?,
            },
            progress,
            &mut timings,
        )
        .await?;
        timings.total = total_started.elapsed();
        Ok((out, timings))
    }
}

impl<W: WorldTree, S: SekaiStore> WorldHandleMut<'_, W, S>
where
    AppError: From<W::Error>,
{
    /// Compute AST diff for one chunk; see [`WorldHandle::diff_world_chunk`].
    pub async fn diff_world_chunk(
        &self,
        commit: Option<CommitId>,
        coord: &ChunkCoord,
        ignore: Option<&[&str]>,
    ) -> Result<Vec<NbtDiffEntry>, AppError> {
        self.view().diff_world_chunk(commit, coord, ignore).await
    }

    /// Compute AST diffs for explicit chunks; see
    /// [`WorldHandle::diff_world_chunks`].
    pub async fn diff_world_chunks(
        &self,
        commit: Option<CommitId>,
        coords: &[ChunkCoord],
        ignore: Option<&[&str]>,
        progress: impl FnMut(DiffProgress) + Send,
    ) -> Result<(Vec<ChunkDiff>, DiffTimings), AppError> {
        self.view()
            .diff_world_chunks(commit, coords, ignore, progress)
            .await
    }
}

trait DiffSource {
    async fn payload(&self, coord: &ChunkCoord) -> Result<Option<Vec<u8>>, AppError>;
}

struct CommitDiff<'a, S> {
    store: &'a S,
    id: CommitId,
}

impl<S: SekaiStore> DiffSource for CommitDiff<'_, S> {
    async fn payload(&self, coord: &ChunkCoord) -> Result<Option<Vec<u8>>, AppError> {
        commit_chunk_compressed(self.store, self.id, coord).await
    }
}

/// One diff run's world side: region handles resolved once, not per chunk.
/// The set is a commit taken when the run starts, so a region created
/// mid-run is treated as absent until the next diff.
struct WorldDiff<R> {
    regions: BTreeMap<RegionKey, R>,
}

impl<R: RegionFile> DiffSource for WorldDiff<R>
where
    AppError: From<R::Error>,
{
    fn payload(
        &self,
        coord: &ChunkCoord,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, AppError>> {
        std::future::ready(read_world_chunk_compressed(&self.regions, coord))
    }
}

/// Diff every coordinate of `old` against `new`, accumulating phase timings.
///
/// A payload of `None` decompresses as [`EMPTY_COMPOUND`], so an absent chunk
/// on one side reports whole-value Added/Removed entries.
async fn diff_coords(
    coords: &[ChunkCoord],
    ignore_set: &[&str],
    old: impl DiffSource,
    new: impl DiffSource,
    mut progress: impl FnMut(DiffProgress) + Send,
    timings: &mut DiffTimings,
) -> Result<Vec<ChunkDiff>, AppError> {
    let mut out = Vec::with_capacity(coords.len());
    for (index, coord) in coords.iter().enumerate() {
        let started = Instant::now();
        let old_compressed = old.payload(coord).await?;
        let new_compressed = new.payload(coord).await?;
        timings.blob_fetch += started.elapsed();
        let started = Instant::now();
        let old_nbt = decompress_or_empty(old_compressed)?;
        let new_nbt = decompress_or_empty(new_compressed)?;
        timings.decompress += started.elapsed();
        let started = Instant::now();
        let entries = diff_nbt(&old_nbt, &new_nbt, ignore_set)?;
        timings.diff_compute += started.elapsed();
        progress(DiffProgress {
            chunks_done: index + 1,
            chunks_total: coords.len(),
        });
        out.push(ChunkDiff {
            coord: *coord,
            entries,
        });
    }
    Ok(out)
}
