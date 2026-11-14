//! Chunk AST diff operations over CAS and metadata stores.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use sekai_anvil::{Chunk, ChunkHashError, ChunkPayloadDiff};
use sekai_core::{
    BlobStore as _, ChunkCoord, ChunkHash, CommitId, MetaStore, RegionKey, RegionKind,
};
use sekai_storage::SekaiStore;
use sekai_world::{Region, RegionFile, WorldLockMode, WorldTree};

use crate::error::AppError;
use crate::instance::{SekaiInstance, WorldGuard, WorldHandle, WorldHandleMut};
use crate::progress::{BuildProgress, Progress};
use crate::regions::discover_regions;

/// Per-phase timings for chunk AST diffs. Informational only; never
/// changes diff semantics.
#[derive(Debug, Clone, Default)]
pub struct DiffTimings {
    /// Wall-clock total.
    pub total: Duration,
    /// Commit metadata lookup plus world-side region read, both sides.
    pub blob_fetch: Duration,
    /// World-side payload decompression and digesting.
    ///
    /// A commit side contributes nothing here: its digest was recorded at
    /// commit time and is read back, not recomputed.
    pub decompress: Duration,
    /// Digest comparison.
    pub diff_compute: Duration,
}

impl<S: SekaiStore> SekaiInstance<S> {
    /// Compute AST diff between two stored chunk blob hashes.
    pub async fn diff_blobs(
        &self,
        old_hash: &ChunkHash,
        new_hash: &ChunkHash,
    ) -> Result<ChunkPayloadDiff, AppError> {
        let mut old_compressed = Vec::new();
        let mut new_compressed = Vec::new();

        self.store
            .blob()
            .fetch_into(old_hash, &mut old_compressed)
            .await?;
        self.store
            .blob()
            .fetch_into(new_hash, &mut new_compressed)
            .await?;

        let old = Chunk::new(RegionKind::Region, 0, 0, &old_compressed).normalized_hash()?;
        let new = Chunk::new(RegionKind::Region, 0, 0, &new_compressed).normalized_hash()?;
        Ok(ChunkPayloadDiff::from_sides(Some(old), Some(new)))
    }
}

/// Digest a world chunk as `commit` would have.
///
/// An absent region file or chunk entry is [`None`]; a corrupt file, payload,
/// or undecodable chunk fails loudly. A digest that is returned goes through
/// [`chunk_digest`], so it equals the CAS key ingest recorded.
fn read_world_chunk<R: RegionFile>(regions: &BTreeMap<RegionKey, R>, coord: &ChunkCoord) -> Side
where
    AppError: From<R::Error>,
{
    let key = RegionKey::of(*coord);
    let Some(region) = regions.get(&key) else {
        return Side::absent();
    };

    let read = region.read(key).map_err(AppError::from);
    let image = match read {
        Ok(image) => image,
        Err(error) => return Side::failed(error),
    };
    let payload = match image.chunk_payload(coord.x, coord.z) {
        Ok(Some(payload)) => payload,
        Ok(None) => return Side::absent(),
        Err(error) => return Side::failed(error.into()),
    };

    let started = Instant::now();
    let digest = chunk_digest(*coord, payload);
    Side {
        digest: digest.map(Some),
        prepare: started.elapsed(),
    }
}

/// The normalized digest - the same key ingest records - with a decode
/// failure attributed to `coord`.
///
/// Knowing which chunk (and so which region file) is malformed is what makes
/// the report actionable. There is no byte-level fallback for a `region/`
/// payload that does not decode: it has no normalized view, so it is
/// reported rather than silently compared by its exact bytes.
fn chunk_digest(coord: ChunkCoord, payload: &[u8]) -> Result<ChunkHash, AppError> {
    Chunk::new(coord.kind, coord.x, coord.z, payload)
        .normalized_hash()
        .map_err(|source| {
            let (rx, rz) = (coord.region_x(), coord.region_z());
            match source {
                ChunkHashError::Decompress(source) => AppError::Anvil(source),
                ChunkHashError::Invalid(source) if source.is_truncated() => {
                    AppError::TruncatedChunk {
                        coord,
                        rx,
                        rz,
                        source,
                    }
                }
                source => AppError::UnhashableChunk {
                    coord,
                    rx,
                    rz,
                    source,
                },
            }
        })
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
            .diff_chunks(old_commit, new_commit, &[*coord], &())
            .await?;
        Ok(diffs.into_iter().next().map_or_else(
            || ChunkPayloadDiff::from_sides(None, None),
            |diff| diff.entries,
        ))
    }
}

/// One chunk's AST diff between two sources.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ChunkDiff {
    /// Chunk the diff belongs to.
    pub coord: ChunkCoord,
    /// Normalized change detection: both sides' digests, `None` for a side
    /// that holds no chunk.
    pub entries: ChunkPayloadDiff,
}

impl<S: SekaiStore> SekaiInstance<S> {
    /// Compute AST diffs for explicit chunk coordinates.
    ///
    /// Compares two commits in the store, additionally returning
    /// per-phase timings. Both sides compare the digests already recorded at
    /// commit time, so no blob is fetched or decoded. A chunk absent (or
    /// tombstoned) on both sides yields an empty [`ChunkPayloadDiff`]; present
    /// on exactly one side keeps `None` for the other. `progress_builder`
    /// counts `coords`.
    pub async fn diff_chunks(
        &self,
        old_commit: CommitId,
        new_commit: CommitId,
        coords: &[ChunkCoord],
        progress_builder: &impl BuildProgress,
    ) -> Result<(Vec<ChunkDiff>, DiffTimings), AppError> {
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
            progress_builder,
            &mut timings,
        )
        .await?;
        timings.total = total_started.elapsed();
        Ok((out, timings))
    }

    /// Effective chunk coordinates of one commit (fallback-resolved set).
    pub async fn commit_chunk_coords(&self, commit: CommitId) -> Result<Vec<ChunkCoord>, AppError> {
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

/// The digest a commit recorded for a chunk.
///
/// Returns `None` when the chunk is unknown there or tombstoned. Nothing is
/// fetched from CAS: the recorded digest *is* what a recomputed one compares
/// against, which is what makes a commit-side diff a metadata lookup.
async fn commit_chunk_hash<S: SekaiStore>(
    store: &S,
    commit_id: CommitId,
    coord: &ChunkCoord,
) -> Result<Option<ChunkHash>, AppError> {
    let Some(entry) = store.meta().lookup_chunk(commit_id, coord).await? else {
        return Ok(None);
    };
    let Some(hash) = entry.blob else {
        return Ok(None);
    };

    Ok(Some(hash))
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
        let (diffs, _) = self.diff_world_chunks(commit, &[*coord], &()).await?;
        Ok(diffs.into_iter().next().map_or_else(
            || ChunkPayloadDiff::from_sides(None, None),
            |diff| diff.entries,
        ))
    }

    /// Compute AST diffs for explicit chunk coordinates between the bound
    /// world state and a commit, additionally returning per-phase timings.
    ///
    /// If `commit` is `None`, the active ref's target is used - the
    /// commit the next one would continue. The commit side is a metadata
    /// lookup; the world side (region lookup, read, decompression, digesting)
    /// counts under `blob_fetch`, and comparing the two digests under
    /// `diff_compute`. `progress_builder` counts `coords`.
    pub async fn diff_world_chunks(
        &self,
        commit: Option<CommitId>,
        coords: &[ChunkCoord],
        progress_builder: &impl BuildProgress,
    ) -> Result<(Vec<ChunkDiff>, DiffTimings), AppError> {
        // Held across resolution and the reads below: the world snapshot
        // must outlive the operation.
        let guard = WorldGuard::acquire(self.world, WorldLockMode::Shared)?;
        let total_started = Instant::now();
        let commit_id = match commit {
            Some(id) => id,
            None => self
                .instance
                .current_commit_id()
                .await?
                .ok_or(AppError::NoCommits)?,
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
            progress_builder,
            &mut timings,
        )
        .await?;
        timings.total = total_started.elapsed();
        drop(guard);
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
        progress_builder: &impl BuildProgress,
    ) -> Result<(Vec<ChunkDiff>, DiffTimings), AppError> {
        self.view()
            .diff_world_chunks(commit, coords, progress_builder)
            .await
    }
}

/// What one source has to say about a chunk.
struct Side {
    /// The [`ChunkHash`] the side's payload digests to, `None` when it holds
    /// no chunk there. A read error confined to this coordinate takes its
    /// place, so a failing chunk can still be reported as absent elsewhere.
    digest: Result<Option<ChunkHash>, AppError>,
    /// Time spent turning the stored payload into that digest: decompression
    /// and hashing. Zero for a recorded digest.
    prepare: Duration,
}

impl Side {
    /// No chunk on this side.
    const fn absent() -> Self {
        Self {
            digest: Ok(None),
            prepare: Duration::ZERO,
        }
    }

    /// The payload exists but could not be read.
    const fn failed(error: AppError) -> Self {
        Self {
            digest: Err(error),
            prepare: Duration::ZERO,
        }
    }
}

trait DiffSource {
    /// The side's answer for `coord`; the outer `Result` is the source itself
    /// failing, which no coordinate confines.
    async fn payload(&self, coord: &ChunkCoord) -> Result<Side, AppError>;
}

struct CommitDiff<'a, S> {
    store: &'a S,
    id: CommitId,
}

impl<S: SekaiStore> DiffSource for CommitDiff<'_, S> {
    async fn payload(&self, coord: &ChunkCoord) -> Result<Side, AppError> {
        Ok(Side {
            digest: commit_chunk_hash(self.store, self.id, coord).await,
            // The digest is recorded, not recomputed: nothing to prepare.
            prepare: Duration::ZERO,
        })
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
    fn payload(&self, coord: &ChunkCoord) -> impl Future<Output = Result<Side, AppError>> {
        // A world read failure is always confined to this coordinate.
        std::future::ready(Ok(read_world_chunk(&self.regions, coord)))
    }
}

/// Diff every coordinate of `old` against `new`, accumulating phase timings.
///
/// Each side yields the digest it holds for the chunk, either recorded at
/// commit time or recomputed from the world; see
/// [`ChunkPayloadDiff::from_sides`] for the cases. Both sides are always
/// consulted before either failure is reported, so a per-chunk read error
/// still leaves the other side's outcome known.
async fn diff_coords(
    coords: &[ChunkCoord],
    old: impl DiffSource,
    new: impl DiffSource,
    progress_builder: &impl BuildProgress,
    timings: &mut DiffTimings,
) -> Result<Vec<ChunkDiff>, AppError> {
    let mut progress = progress_builder.build_progress(coords.len());
    let mut out = Vec::with_capacity(coords.len());
    for coord in coords {
        let started = Instant::now();
        let old_side = old.payload(coord).await?;
        let new_side = new.payload(coord).await?;
        timings.blob_fetch += started.elapsed();
        timings.decompress += old_side.prepare + new_side.prepare;
        let started = Instant::now();
        let entries = ChunkPayloadDiff::from_sides(old_side.digest?, new_side.digest?);
        timings.diff_compute += started.elapsed();
        progress.step();
        out.push(ChunkDiff {
            coord: *coord,
            entries,
        });
    }
    Ok(out)
}
