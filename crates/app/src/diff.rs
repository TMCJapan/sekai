//! Chunk AST diff operations over CAS and metadata stores.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use sekai_core::{
    BlobHash, BlobStore as _, ChunkCoord, CommitId, DiffHash, MetaStore, RegionKey, RegionKind,
    diff_nbt,
};
use sekai_nbt::nanonbt::Cesu8Buf;
use sekai_nbt::{ChunkPayload, ChunkPayloadDiff, Heightmaps};
use sekai_storage::{BlockingBlobStore, SekaiStore, StoreLockMode};
use sekai_world::{Region, RegionFile, WorldLockMode, WorldTree};

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
    ) -> Result<ChunkPayloadDiff, AppError> {
        let _lock = self.lock_store(StoreLockMode::Shared)?;
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

        Ok(diff_nbt(&old_nbt, &new_nbt)?)
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

    let image = region.read(key)?;

    let payload = image.chunk_payload(coord.x, coord.z)?;
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
    ) -> Result<ChunkPayloadDiff, AppError> {
        let (diffs, _) = self
            .diff_chunks(old_commit, new_commit, &[*coord], |_| {})
            .await?;
        Ok(diffs
            .into_iter()
            .next()
            .map_or(ChunkPayloadDiff::Nothing, |diff| diff.entries))
    }
}

/// One chunk's AST diff between two sources.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ChunkDiff {
    /// Chunk the diff belongs to.
    pub coord: ChunkCoord,
    /// Normalized change detection: `Nothing` when both sides normalize
    /// identically, `Full` with both hash pairs otherwise.
    pub entries: ChunkPayloadDiff,
}

impl<S: SekaiStore> SekaiInstance<S> {
    /// Compute AST diffs for explicit chunk coordinates.
    ///
    /// Compares two commits in the store, additionally returning
    /// per-phase timings. A chunk absent (or tombstoned) on a side diffs as a
    /// synthesized zeroed payload there: absent on both sides yields
    /// [`ChunkPayloadDiff::Nothing`], present on one side reports
    /// [`ChunkPayloadDiff::Full`].
    pub async fn diff_chunks(
        &self,
        old_commit: CommitId,
        new_commit: CommitId,
        coords: &[ChunkCoord],
        progress: impl FnMut(DiffProgress) + Send,
    ) -> Result<(Vec<ChunkDiff>, DiffTimings), AppError> {
        let _lock = self.lock_store(StoreLockMode::Shared)?;
        let total_started = Instant::now();
        let mut timings = DiffTimings::default();
        let out = diff_coords(
            coords,
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
        let image = region.read(key)?;
        image.visit_chunks(|chunk| {
            coords.push(ChunkCoord::new(key.dim, key.kind, chunk.x, chunk.z));
            true
        })?;
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

/// The NBT a missing chunk diffs as: a synthesized zeroed [`ChunkPayload`]
/// (data version 0, empty status). Absent on both sides yields
/// [`ChunkPayloadDiff::Nothing`]; present on one side reports a
/// [`ChunkPayloadDiff::Full`] against those zeroed bytes.
fn decompress_or_empty(compressed: Option<Vec<u8>>) -> Result<Vec<u8>, AppError> {
    compressed.map_or_else(
        || {
            let chunk = ChunkPayload {
                data_version: 0,
                x_pos: 0,
                y_pos: 0,
                z_pos: 0,
                inhabited_time: 0,
                last_update: 0,
                sections: vec![],
                heightmaps: Heightmaps::default(),
                status: &Cesu8Buf::new(),
                block_entities: vec![],
                block_ticks: vec![],
                fluid_ticks: vec![],
            };
            sekai_nbt::nanonbt::to_bytes(&chunk).map_err(AppError::Nbt)
        },
        |c| decompress_chunk(&c),
    )
}

/// Diff for non-level-chunk kinds (`entities`/`poi`), whose payload schema is
/// not [`ChunkPayload`]. Raw NBT bytes are hashed independently so a `Full`
/// result never carries identical hashes.
fn raw_nbt_diff(old: &[u8], new: &[u8]) -> ChunkPayloadDiff {
    if old == new {
        return ChunkPayloadDiff::Nothing;
    }
    ChunkPayloadDiff::Full {
        old: DiffHash(*blake3::hash(old).as_bytes()),
        new: DiffHash(*blake3::hash(new).as_bytes()),
    }
}

impl<W: WorldTree, S: SekaiStore> WorldHandle<'_, W, S>
where
    AppError: From<W::Error>,
{
    /// Compute AST diff for a chunk coordinate between the bound world state
    /// and a commit.
    ///
    /// If `commit` is `None`, the active ref's target is used; an unborn
    /// active ref fails with [`AppError::NoCommits`].
    pub async fn diff_world_chunk(
        &self,
        commit: Option<CommitId>,
        coord: &ChunkCoord,
    ) -> Result<ChunkPayloadDiff, AppError> {
        let (diffs, _) = self.diff_world_chunks(commit, &[*coord], |_| {}).await?;
        Ok(diffs
            .into_iter()
            .next()
            .map_or(ChunkPayloadDiff::Nothing, |diff| diff.entries))
    }

    /// Compute AST diffs for explicit chunk coordinates between the bound
    /// world state and a commit, additionally returning per-phase timings.
    ///
    /// If `commit` is `None`, the active ref's target is used - the
    /// commit the next one would continue. World-side acquisition
    /// (region lookup, read, parse) counts under `blob_fetch`; both
    /// sides' decompression under `decompress`.
    pub async fn diff_world_chunks(
        &self,
        commit: Option<CommitId>,
        coords: &[ChunkCoord],
        progress: impl FnMut(DiffProgress) + Send,
    ) -> Result<(Vec<ChunkDiff>, DiffTimings), AppError> {
        // Held across resolution and the reads below: the store and world
        // snapshots must outlive the operation.
        let locks = OperationLocks::acquire(
            self.instance,
            StoreLockMode::Shared,
            self.world,
            WorldLockMode::Shared,
        )?;
        let total_started = Instant::now();
        let commit_id = match commit {
            Some(id) => id,
            None => {
                // The outer lock is Shared and `current_commit_id` takes
                // Shared too; Shared + Shared is compatible.
                self.instance
                    .current_commit_id()
                    .await?
                    .ok_or(AppError::NoCommits)?
            }
        };

        let mut timings = DiffTimings::default();
        let out = diff_coords(
            coords,
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
        drop(locks);
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
    ) -> Result<ChunkPayloadDiff, AppError> {
        self.view().diff_world_chunk(commit, coord).await
    }

    /// Compute AST diffs for explicit chunks; see
    /// [`WorldHandle::diff_world_chunks`].
    pub async fn diff_world_chunks(
        &self,
        commit: Option<CommitId>,
        coords: &[ChunkCoord],
        progress: impl FnMut(DiffProgress) + Send,
    ) -> Result<(Vec<ChunkDiff>, DiffTimings), AppError> {
        self.view()
            .diff_world_chunks(commit, coords, progress)
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
/// The set is captured when the run starts, so a region created mid-run
/// is treated as absent until the next diff.
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
/// A payload of `None` decompresses as a synthesized zeroed chunk (see
/// [`decompress_or_empty`]), so an absent chunk on one side reports
/// [`ChunkPayloadDiff::Full`] against those bytes.
async fn diff_coords(
    coords: &[ChunkCoord],
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
        let entries = match diff_nbt(&old_nbt, &new_nbt) {
            Ok(diff) => diff,
            // `entities`/`poi` payloads use their own schemas; compare their
            // raw bytes. A level chunk with missing fields is corruption and
            // must fail loudly.
            Err(sekai_nbt::NbtError::MissingField(_)) if coord.kind != RegionKind::Region => {
                raw_nbt_diff(&old_nbt, &new_nbt)
            }
            Err(e) => return Err(AppError::Nbt(e)),
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_nbt_diff_hashes_both_sides() {
        let changed = raw_nbt_diff(b"old", b"new");
        assert!(
            matches!(changed, ChunkPayloadDiff::Full { old, new } if old != new),
            "different payloads need distinct hashes: {changed:?}"
        );
        assert_eq!(raw_nbt_diff(b"same", b"same"), ChunkPayloadDiff::Nothing);
    }
}
