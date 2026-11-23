//! The region-image cache: best-effort change proofs and commit-free
//! population (ADR-0029, ADR-0033, ADR-0034).
//!
//! An entry stores a fully covered file under the region node hash its
//! own chunks produce, and only when the image it read produced exactly
//! that hash. So a live file whose bytes hash to the cached image digest
//! holds exactly the chunks the committed region node names: the whole
//! region is proven unchanged without decompressing or normalizing a
//! single chunk.
//!
//! The proof side ([`prove_regions_unchanged`]) is an optimization, never a
//! decision: a cache miss, a byte mismatch, or any read failure leaves the
//! region unproven and the caller falls back to its per-chunk path. It
//! never writes to the cache.
//!
//! The population side ([`WorldHandleMut::update_cache`]) is the only
//! writer: it keys each file under the region node its own chunks produce,
//! so the entry is sound by construction and ready for the next commit
//! that names it. Commit itself records history only; `sekai commit` runs
//! the update pass before recording (ADR-0034).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sekai_core::{ChunkCoord, CommitEntry, CommitId, RegionHash, RegionKey, hash_bytes};
use sekai_storage::{CacheStore, MetaStore as _, RegionNode, SekaiStore};
use sekai_util::{JobSet as _, ThreadRunner};
use sekai_world::{Region, RegionFile, WorldError, WorldLock, WorldLockMode, WorldTree};

use crate::error::AppError;
use crate::instance::{WorldGuard, WorldHandleMut};
use crate::progress::{BuildProgress, Progress};
use crate::regions::discover_regions;
use crate::workers::worker_error;

/// One file's proof task: the committed region hash, the cached image
/// digest to compare against, and the discovered handle to re-read.
struct Candidate<R> {
    key: RegionKey,
    region: RegionHash,
    mca_hash: [u8; 32],
    handle: R,
}

/// Prove which of `keys`' live region files are unchanged since `commit`,
/// returning the committed region node of each proven file.
///
/// `regions` holds the discovered handles; a key with no handle (the file
/// is gone) stays unproven. Only the cheap cache digest is queried first -
/// a file is re-read raw, never chunk-decoded, only when an entry exists.
///
/// Best-effort by design: every failure - a tree read, a cache query, a
/// file re-read, a worker panic - only costs the proof, never correctness.
/// The caller answers unproven regions through its full per-chunk path.
pub(crate) async fn prove_regions_unchanged<R, S, L>(
    runner: &impl ThreadRunner,
    store: &S,
    commit: CommitId,
    regions: &BTreeMap<RegionKey, R>,
    keys: &[RegionKey],
    guard: &Arc<WorldGuard<L>>,
) -> BTreeMap<RegionKey, RegionNode>
where
    R: RegionFile<Error = WorldError>,
    S: SekaiStore,
    L: WorldLock,
{
    let Ok(hashes) = sekai_storage::tree::commit_region_hashes(store.meta(), commit, keys).await
    else {
        return BTreeMap::new();
    };
    let mut candidates = Vec::new();
    for (key, region) in hashes {
        let Ok(Some(mca_hash)) = store.cache().mca_hash(region).await else {
            continue;
        };
        let Some(handle) = regions.get(&key) else {
            continue;
        };
        candidates.push(Candidate {
            key,
            region,
            mca_hash,
            handle: handle.clone(),
        });
    }

    // Same bounded fan-out as ingest: a finished file frees its slot. The
    // raw reads run on the runner's threads, each holding a share of the
    // world lock until it ends (ADR-0022).
    let proof_guard = Arc::clone(guard);
    let mut jobs = runner.run(candidates, move |candidate| {
        let _guard = Arc::clone(&proof_guard);
        let image = candidate.handle.read(candidate.key).ok()?;
        (hash_bytes(image.image()) == candidate.mca_hash)
            .then_some((candidate.key, candidate.region))
    });
    let mut proven = Vec::new();
    while let Some(joined) = jobs.join_next().await {
        if let Ok(Some(pair)) = joined {
            proven.push(pair);
        }
    }
    if proven.is_empty() {
        return BTreeMap::new();
    }
    // Completion order is nondeterministic; keep the bulk node read and
    // the returned map in region order.
    proven.sort_by_key(|(key, _)| *key);
    let hashes: Vec<RegionHash> = proven.iter().map(|(_, hash)| *hash).collect();
    let Ok(nodes) = store.meta().read_region_nodes(&hashes).await else {
        return BTreeMap::new();
    };
    proven
        .into_iter()
        .zip(nodes)
        .map(|((key, _), node)| (key, node))
        .collect()
}

/// Cache-update behavior knobs.
#[derive(Debug, Clone, Copy, Default)]
pub struct CacheUpdateOptions {}

/// What a cache update populated, without recording a commit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheUpdateReport {
    /// Region files visited.
    pub files: usize,
    /// Chunks hashed across those files.
    pub chunks: usize,
    /// Cache entries newly inserted.
    pub entries: usize,
    /// Entries already present: first-image-wins kept the original.
    pub existing: usize,
    /// Files holding no chunks, which name no region node and cache nothing.
    pub empty: usize,
}

/// Per-phase timings. Informational only; never changes update semantics.
#[derive(Debug, Clone, Default)]
pub struct CacheUpdateTimings {
    /// Wall-clock total.
    pub total: Duration,
    /// World discovery.
    pub discover: Duration,
    /// Reading and hashing the region files.
    pub ingest: Duration,
    /// Subset of `ingest` spent hashing chunks.
    pub hash: Duration,
    /// Cache writes plus the advisory flush.
    pub cache: Duration,
}

impl<W: WorldTree<Error = WorldError>, S: SekaiStore, T: ThreadRunner> WorldHandleMut<'_, W, S, T> {
    /// Populate the region-image cache from the live world without
    /// recording a commit (ADR-0033). This is the cache's only population
    /// path: commit records history only, and `sekai commit` runs this
    /// pass before recording (ADR-0034).
    ///
    /// Every discovered file is read and its chunks normalized exactly as
    /// commit ingest does, but nothing reaches the CAS or the metadata: the
    /// only writes are cache entries. Each file is keyed under the region
    /// node its own chunks produce ([`file_region_hash`]), the same hash a
    /// full commit of the unchanged file would name, so the entry is sound
    /// by construction - no committed tree is needed to confirm it, and the
    /// run works on an unborn store too.
    ///
    /// The image stays in hand from the read through the cache write: the
    /// bytes hashed are the bytes stored and no verifying re-read is
    /// needed. Memory stays bounded by the fan-out, one image per worker.
    ///
    /// The world stays stable for the read pass under a shared lock.
    /// `progress_builder` counts files as they complete. Hashing a corrupt
    /// or unsupported payload fails the run loudly, like ingest; a cache
    /// write failure fails it too, since updating the cache is the whole
    /// point of the command.
    ///
    /// [`file_region_hash`]: sekai_storage::tree::file_region_hash
    pub async fn update_cache(
        &mut self,
        _options: CacheUpdateOptions,
        progress_builder: &impl BuildProgress,
    ) -> Result<(CacheUpdateReport, CacheUpdateTimings), AppError> {
        let total = Instant::now();
        let guard = WorldGuard::acquire(self.world, WorldLockMode::Shared)?;
        let runner = &self.instance.runner;
        let store = &mut self.instance.store;

        // Discovery walks directories on the runner, owning a share of the
        // world lock like every file pass.
        let discover_started = Instant::now();
        let world_handle = self.world.clone();
        let discover_guard = Arc::clone(&guard);
        let regions = runner
            .call(move || {
                let _guard = discover_guard;
                discover_regions(&world_handle)
            })
            .await
            .map_err(worker_error("cache update worker"))??;
        let discover = discover_started.elapsed();

        let mut progress = progress_builder.build_progress(regions.len());
        let mut report = CacheUpdateReport {
            files: regions.len(),
            ..Default::default()
        };
        let mut ingest = Duration::ZERO;
        let mut hash = Duration::ZERO;
        let mut cache = Duration::ZERO;

        // Same bounded fan-out as ingest: a finished file frees its slot.
        // Reads and hashing run on the runner's threads; the cache writes
        // stay here on the caller's executor, interleaved with the joins.
        // Puts are insert-if-absent, so the sequential writes never
        // conflict with anything.
        let mut adapter = store.cache().clone();
        let hash_guard = Arc::clone(&guard);
        let mut jobs = runner.run(regions, move |(key, region)| {
            let _guard = Arc::clone(&hash_guard);
            hash_region(&region, key)
        });
        // Completion order surfaces a failed file at once; returning drops
        // the set, which stops the files still queued. Entries already
        // written stay: the cache is append-only derived state.
        while let Some(joined) = jobs.join_next().await {
            let hashed = joined.map_err(worker_error("cache update worker"))??;
            let (inserted, put) = if let Some(region_hash) = hashed.region {
                let started = Instant::now();
                let inserted = adapter
                    .put(region_hash, hashed.mca_hash, &hashed.image)
                    .await?;
                (inserted, started.elapsed())
            } else {
                (false, Duration::ZERO)
            };
            progress.step();
            report.chunks += hashed.chunks;
            if hashed.region.is_none() {
                report.empty += 1;
            } else if inserted {
                report.entries += 1;
            } else {
                report.existing += 1;
            }
            ingest += hashed.open + hashed.hash;
            hash += hashed.hash;
            cache += put;
        }

        // Advisory flush: the cache is derived, so this keeps the log small
        // rather than guarding a crash order.
        let sync_started = Instant::now();
        store.cache_mut().sync().await?;
        cache += sync_started.elapsed();

        let timings = CacheUpdateTimings {
            total: total.elapsed(),
            discover,
            ingest,
            hash,
            cache,
        };
        Ok((report, timings))
    }
}

/// One file read, hashed, and retained for its cache write.
struct HashedRegion {
    /// Region node the file's own chunks produce; `None` when it holds none.
    region: Option<RegionHash>,
    /// Content hash of the raw image.
    mca_hash: [u8; 32],
    /// The undecomposed bytes, kept so the cache stores exactly what was
    /// hashed - no verifying re-read is needed without a commit barrier.
    image: Vec<u8>,
    /// Chunks hashed.
    chunks: usize,
    /// File open time.
    open: Duration,
    /// Chunk hashing time.
    hash: Duration,
}

/// Read one region file, hash the raw image and every chunk, and derive the
/// region node the chunks produce. A payload with no normalized view fails
/// the file with its coordinates attached, exactly like commit ingest.
fn hash_region<R: RegionFile<Error = WorldError>>(
    region: &R,
    key: RegionKey,
) -> Result<HashedRegion, AppError> {
    let opened = Instant::now();
    let image = region.read(key)?;
    let open = opened.elapsed();
    let mca_hash = hash_bytes(image.image());

    let mut entries = Vec::new();
    let mut failure = None;
    let hash_started = Instant::now();
    image.visit_chunks(|chunk| {
        let coord = ChunkCoord::new(key.dim, key.kind, chunk.x, chunk.z);
        match chunk.normalized_hash() {
            Ok(hash) => {
                entries.push(CommitEntry::new(coord, Some(hash)));
                true
            }
            Err(source) => {
                failure = Some(match source {
                    sekai_anvil::ChunkHashError::Decompress(source) => AppError::Anvil(source),
                    source => AppError::UnhashableChunk {
                        coord,
                        rx: key.rx,
                        rz: key.rz,
                        source,
                    },
                });
                false
            }
        }
    })?;
    if let Some(error) = failure {
        return Err(error);
    }
    let hash = hash_started.elapsed();
    let chunks = entries.len();
    let region = sekai_storage::tree::file_region_hash(&entries);
    Ok(HashedRegion {
        region,
        mca_hash,
        image: image.image().to_vec(),
        chunks,
        open,
        hash,
    })
}
