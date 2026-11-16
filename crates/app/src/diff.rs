//! Chunk AST diff operations over CAS and metadata stores.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sekai_anvil::{Chunk, ChunkHashError, ChunkPayloadDiff};
use sekai_core::{ChunkCoord, ChunkHash, CommitId, RegionKey, RegionKind};
use sekai_storage::{BlobStore as _, MetaStore, RegionNode, SekaiStore, StorageError};
use sekai_world::{Region, RegionFile, WorldLock, WorldLockMode, WorldTree};
use tokio::task::JoinSet;

use crate::cache::prove_regions_unchanged;
use crate::error::AppError;
use crate::instance::{SekaiInstance, WorldGuard, WorldHandle, WorldHandleMut};
use crate::progress::{BuildProgress, Progress};
use crate::regions::discover_regions;
use crate::workers::worker_slots;

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
    /// counts the region files `coords` span, stepping as each file's last
    /// chunk completes.
    pub async fn diff_chunks(
        &self,
        old_commit: CommitId,
        new_commit: CommitId,
        coords: &[ChunkCoord],
        progress_builder: &impl BuildProgress,
    ) -> Result<(Vec<ChunkDiff>, DiffTimings), AppError> {
        let total_started = Instant::now();
        let mut timings = DiffTimings::default();
        // Both sides are recorded digests: no world file to prove.
        let proven = BTreeMap::new();
        let out = diff_coords(
            coords,
            CommitDiff {
                store: &self.store,
                id: old_commit,
                proven: &proven,
            },
            CommitDiff {
                store: &self.store,
                id: new_commit,
                proven: &proven,
            },
            progress_builder,
            &mut timings,
        )
        .await?;
        timings.total = total_started.elapsed();
        Ok((out, timings))
    }

    /// Effective chunk coordinates of one commit (snapshot set).
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
        region.visit_present(key, |x, z| {
            coords.push(ChunkCoord::new(key.dim, key.kind, x, z));
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
    /// active ref diffs against the empty snapshot (the chunk is added).
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
    /// commit the next one would continue. An unborn active ref (a store with
    /// no commit to continue) diffs against the empty snapshot: every chunk
    /// the world holds is reported as added. The commit side is a metadata
    /// lookup; the world side reads each region file once and digests its
    /// requested chunks, fanned out across the blocking pool. Decompression
    /// and digesting count under `decompress`, the reads and metadata lookups
    /// under `blob_fetch`, and comparing the two digests under `diff_compute`.
    /// `progress_builder` counts the region files `coords` span, stepping once
    /// per file as it completes.
    ///
    /// Regions proven unchanged through the region-image cache answer both
    /// sides from the committed region node, skipping the world read; the
    /// proof itself counts under `blob_fetch` (ADR-0029).
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
        let base = match commit {
            Some(id) => Some(id),
            None => self.instance.current_commit_id().await?,
        };

        let regions = discover_regions(self.world)?;
        let mut timings = DiffTimings::default();

        // Group the requested coordinates by region, keeping their positions
        // so the result stays in input order however the reads complete.
        let mut by_region: BTreeMap<RegionKey, Vec<usize>> = BTreeMap::new();
        for (index, coord) in coords.iter().enumerate() {
            by_region.entry(RegionKey::of(*coord)).or_default().push(index);
        }

        // Only the regions `coords` span are worth proving: a single-chunk
        // diff must not re-hash the whole world.
        let prepass_started = Instant::now();
        let keys: Vec<RegionKey> = by_region.keys().copied().collect();
        let proven = match base {
            Some(id) => {
                prove_regions_unchanged(&self.instance.store, id, &regions, &keys, &guard).await
            }
            None => BTreeMap::new(),
        };
        timings.blob_fetch += prepass_started.elapsed();

        // Commit side: recorded digests, indexed by coordinate position.
        let mut old_side: Vec<Option<ChunkHash>> = vec![None; coords.len()];
        commit_sides(
            &self.instance.store,
            base,
            coords,
            &by_region,
            &proven,
            &mut old_side,
            &mut timings,
        )
        .await?;

        // World side: one file read per unproven region; proven regions answer
        // from their committed node without a read.
        let mut new_side: Vec<Option<ChunkHash>> = vec![None; coords.len()];
        let mut progress = progress_builder.build_progress(by_region.len());
        world_sides(
            &regions,
            coords,
            &by_region,
            &proven,
            &mut new_side,
            &guard,
            &mut progress,
            &mut timings,
        )
        .await?;

        let combine_started = Instant::now();
        let out = coords
            .iter()
            .zip(old_side.iter().zip(new_side.iter()))
            .map(|(coord, (old, new))| ChunkDiff {
                coord: *coord,
                entries: ChunkPayloadDiff::from_sides(*old, *new),
            })
            .collect();
        timings.diff_compute += combine_started.elapsed();
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

/// Above this many unproven coordinates the commit side answers from one
/// materialized tree walk instead of a metadata lookup per chunk.
const BULK_COMMIT_THRESHOLD: usize = 1024;

/// Diff fans out one task per region file. The work is dominated by
/// decompression and hashing, so oversubscribing the blocking pool keeps
/// every CPU busy while some tasks wait on reads.
fn diff_worker_slots() -> usize {
    worker_slots(0).saturating_mul(2)
}

/// One region's world-side outcome: each requested coordinate's digest
/// with its position in the input, plus the phase timings of the run that
/// produced them.
struct RegionSide {
    digests: Vec<(usize, Option<ChunkHash>)>,
    read: Duration,
    prepare: Duration,
}

/// Fill the commit side: recorded digests, indexed by input position.
///
/// An unborn base (`None`) is the empty snapshot: every coordinate stays
/// absent. Proven regions answer from their committed node; the rest come
/// from metadata lookups, or one bulk tree walk when there are enough of
/// them to amortize materializing the snapshot.
async fn commit_sides<S: SekaiStore>(
    store: &S,
    base: Option<CommitId>,
    coords: &[ChunkCoord],
    by_region: &BTreeMap<RegionKey, Vec<usize>>,
    proven: &BTreeMap<RegionKey, RegionNode>,
    old_side: &mut [Option<ChunkHash>],
    timings: &mut DiffTimings,
) -> Result<(), AppError> {
    let Some(id) = base else {
        return Ok(());
    };
    let started = Instant::now();
    let mut unproven = Vec::new();
    for (key, indices) in by_region {
        if let Some(node) = proven.get(key) {
            for &index in indices {
                let (cx, cz) = local_slot(coords[index]);
                old_side[index] = node.get(cx, cz);
            }
        } else {
            unproven.extend(indices.iter().copied());
        }
    }
    if unproven.len() >= BULK_COMMIT_THRESHOLD {
        let needed: BTreeSet<ChunkCoord> = unproven.iter().map(|&i| coords[i]).collect();
        let mut recorded = BTreeMap::new();
        store
            .meta()
            .visit_commit_chunks(id, |entry| {
                if needed.contains(&entry.coord) {
                    recorded.insert(entry.coord, entry.blob);
                }
                true
            })
            .await?;
        for index in unproven {
            old_side[index] = recorded.get(&coords[index]).copied().flatten();
        }
    } else {
        for index in unproven {
            old_side[index] = commit_chunk_hash(store, id, &coords[index]).await?;
        }
    }
    timings.blob_fetch += started.elapsed();
    Ok(())
}

/// Fill the world side, stepping `progress` once per region file.
///
/// Proven regions answer from their committed node and step at once; a
/// region whose file is gone leaves its coordinates absent and steps too.
/// Every remaining file is read once on the blocking pool, its requested
/// chunks digested there; results scatter back into input positions as
/// tasks complete, so the output order never depends on completion order.
#[allow(clippy::too_many_arguments)]
async fn world_sides<R: RegionFile, L: WorldLock>(
    regions: &BTreeMap<RegionKey, R>,
    coords: &[ChunkCoord],
    by_region: &BTreeMap<RegionKey, Vec<usize>>,
    proven: &BTreeMap<RegionKey, RegionNode>,
    new_side: &mut [Option<ChunkHash>],
    guard: &Arc<WorldGuard<L>>,
    progress: &mut impl Progress,
    timings: &mut DiffTimings,
) -> Result<(), AppError>
where
    AppError: From<R::Error>,
{
    let mut spawnable = Vec::new();
    for (key, indices) in by_region {
        if let Some(node) = proven.get(key) {
            for &index in indices {
                let (cx, cz) = local_slot(coords[index]);
                new_side[index] = node.get(cx, cz);
            }
            progress.step();
        } else if let Some(region) = regions.get(key) {
            let items = indices.iter().map(|&i| (i, coords[i])).collect();
            spawnable.push((*key, region.clone(), items));
        } else {
            // The file is gone: every requested chunk there is absent.
            progress.step();
        }
    }

    // Same bounded fan-out as ingest: a finished file frees its slot.
    // Returning on error drops the set, aborting the files still queued.
    let mut queued = spawnable.into_iter();
    let mut set = JoinSet::new();
    for file in queued.by_ref().take(diff_worker_slots()) {
        set.spawn(read_region_side(file, Arc::clone(guard)));
    }
    while let Some(joined) = set.join_next().await {
        let side = joined.map_err(|source| {
            AppError::Storage(StorageError::backend("diff worker", source))
        })??;
        for (index, digest) in side.digests {
            new_side[index] = digest;
        }
        timings.blob_fetch += side.read;
        timings.decompress += side.prepare;
        progress.step();
        if let Some(file) = queued.next() {
            set.spawn(read_region_side(file, Arc::clone(guard)));
        }
    }
    Ok(())
}

/// Read one region file once and digest every requested chunk it holds.
///
/// Runs on the blocking pool holding a share of the world lock until it
/// ends (ADR-0022): an abandoned diff cannot free the world while a file
/// is still being read. The read counts under `blob_fetch`; decompression
/// and digesting under `decompress`.
async fn read_region_side<R: RegionFile, L: WorldLock>(
    file: (RegionKey, R, Vec<(usize, ChunkCoord)>),
    guard: Arc<WorldGuard<L>>,
) -> Result<RegionSide, AppError>
where
    AppError: From<R::Error>,
{
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        let (key, region, items) = file;
        let started = Instant::now();
        let image = region.read(key)?;
        let read = started.elapsed();
        let mut digests = Vec::with_capacity(items.len());
        let mut prepare = Duration::ZERO;
        for (index, coord) in items {
            let started = Instant::now();
            let digest = match image.chunk_payload(coord.x, coord.z)? {
                Some(payload) => Some(chunk_digest(coord, payload)?),
                None => None,
            };
            prepare += started.elapsed();
            digests.push((index, digest));
        }
        Ok(RegionSide {
            digests,
            read,
            prepare,
        })
    })
    .await
    .map_err(|source| AppError::Storage(StorageError::backend("diff worker", source)))?
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

trait DiffSource {
    /// The side's answer for `coord`; the outer `Result` is the source itself
    /// failing, which no coordinate confines.
    async fn payload(&self, coord: &ChunkCoord) -> Result<Side, AppError>;
}

struct CommitDiff<'a, S> {
    store: &'a S,
    id: CommitId,
    /// Regions proven unchanged against `id`: their committed node answers
    /// every coordinate without a metadata lookup.
    proven: &'a BTreeMap<RegionKey, RegionNode>,
}

impl<S: SekaiStore> DiffSource for CommitDiff<'_, S> {
    async fn payload(&self, coord: &ChunkCoord) -> Result<Side, AppError> {
        if let Some(node) = self.proven.get(&RegionKey::of(*coord)) {
            return Ok(proven_side(node, coord));
        }
        Ok(Side {
            digest: commit_chunk_hash(self.store, self.id, coord).await,
            // The digest is recorded, not recomputed: nothing to prepare.
            prepare: Duration::ZERO,
        })
    }
}

/// The answer a proven region's committed node gives for `coord`: the
/// live file holds exactly the node's chunks, so the digest is read, not
/// recomputed.
fn proven_side(node: &RegionNode, coord: &ChunkCoord) -> Side {
    let (cx, cz) = local_slot(*coord);
    Side {
        digest: Ok(node.get(cx, cz)),
        prepare: Duration::ZERO,
    }
}

#[allow(clippy::cast_possible_truncation)]
const fn local_slot(coord: ChunkCoord) -> (u8, u8) {
    // `rem_euclid(32)` is always in `0..32`, so the cast cannot truncate.
    (coord.x.rem_euclid(32) as u8, coord.z.rem_euclid(32) as u8)
}

/// Diff every coordinate of `old` against `new`, accumulating phase timings.
///
/// Each side yields the digest it holds for the chunk, either recorded at
/// commit time or recomputed from the world; see
/// [`ChunkPayloadDiff::from_sides`] for the cases. Both sides are always
/// consulted before either failure is reported, so a per-chunk read error
/// still leaves the other side's outcome known.
///
/// Progress counts region files, as commit does: a file steps once its last
/// coordinate finishes. Coords need not arrive sorted, so the run tracks
/// each file's remaining chunks instead of watching for region boundaries.
async fn diff_coords(
    coords: &[ChunkCoord],
    old: impl DiffSource,
    new: impl DiffSource,
    progress_builder: &impl BuildProgress,
    timings: &mut DiffTimings,
) -> Result<Vec<ChunkDiff>, AppError> {
    let mut pending: BTreeMap<RegionKey, usize> = BTreeMap::new();
    for coord in coords {
        *pending.entry(RegionKey::of(*coord)).or_default() += 1;
    }
    let mut progress = progress_builder.build_progress(pending.len());
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
        if let Some(left) = pending.get_mut(&RegionKey::of(*coord)) {
            *left -= 1;
            if *left == 0 {
                progress.step();
            }
        }
        out.push(ChunkDiff {
            coord: *coord,
            entries,
        });
    }
    Ok(out)
}
