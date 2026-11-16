//! The region-image cache: best-effort change proofs and commit-free
//! population (ADR-0029, ADR-0030, ADR-0033).
//!
//! A commit caches each fully covered file under the region node hash it
//! names, and only when the image it read produced exactly that hash. So
//! a live file whose bytes hash to the cached image digest holds exactly
//! the chunks the committed region node names: the whole region is proven
//! unchanged without decompressing or normalizing a single chunk.
//!
//! The proof side ([`prove_regions_unchanged`]) is an optimization, never a
//! decision: a cache miss, a byte mismatch, or any read failure leaves the
//! region unproven and the caller falls back to its per-chunk path. It
//! never writes to the cache.
//!
//! The population side ([`WorldHandleMut::update_cache`]) runs the same
//! hash-and-store work a commit does, without recording a commit: it keys
//! each file under the region node its own chunks produce, so the entry is
//! sound by construction and ready for the next commit that names it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sekai_core::{ChunkCoord, CommitEntry, CommitId, RegionHash, RegionKey, hash_bytes};
use sekai_storage::{CacheStore, MetaStore as _, RegionNode, SekaiStore, StorageError};
use sekai_world::{Region, RegionFile, WorldLock, WorldLockMode, WorldTree};
use tokio::task::JoinSet;

use crate::error::AppError;
use crate::instance::{WorldGuard, WorldHandleMut};
use crate::progress::{BuildProgress, Progress};
use crate::regions::discover_regions;
use crate::workers::worker_slots;

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
    store: &S,
    commit: CommitId,
    regions: &BTreeMap<RegionKey, R>,
    keys: &[RegionKey],
    guard: &Arc<WorldGuard<L>>,
) -> BTreeMap<RegionKey, RegionNode>
where
    R: RegionFile,
    S: SekaiStore,
    L: WorldLock,
{
    let Ok(hashes) =
        sekai_core::usecase::commit::commit_region_hashes(store.meta(), commit, keys).await
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
    // raw reads run on the blocking pool, each holding a share of the world
    // lock until it ends (ADR-0022).
    let mut queued = candidates.into_iter();
    let mut set = JoinSet::new();
    for candidate in queued.by_ref().take(worker_slots(0)) {
        set.spawn(prove_file(candidate, Arc::clone(guard)));
    }
    let mut proven = Vec::new();
    while let Some(joined) = set.join_next().await {
        if let Ok(Some(pair)) = joined {
            proven.push(pair);
        }
        if let Some(candidate) = queued.next() {
            set.spawn(prove_file(candidate, Arc::clone(guard)));
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

/// Re-read one file raw and compare its content hash against the cached
/// image digest. `None` when the file cannot be read or has changed.
async fn prove_file<R: RegionFile, L: WorldLock>(
    candidate: Candidate<R>,
    guard: Arc<WorldGuard<L>>,
) -> Option<(RegionKey, RegionHash)> {
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        let image = candidate.handle.read(candidate.key).ok()?;
        (hash_bytes(image.image()) == candidate.mca_hash)
            .then_some((candidate.key, candidate.region))
    })
    .await
    .ok()
    .flatten()
}

/// Cache-update behavior knobs.
#[derive(Debug, Clone, Copy, Default)]
pub struct CacheUpdateOptions {
    /// How many files hash at once. `0` means one per CPU, `1` stays
    /// sequential.
    pub concurrency: usize,
}

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

impl<W: WorldTree, S: SekaiStore> WorldHandleMut<'_, W, S>
where
    AppError: From<W::Error>,
{
    /// Populate the region-image cache from the live world without
    /// recording a commit (ADR-0033).
    ///
    /// Every discovered file is read and its chunks normalized exactly as
    /// commit ingest does, but nothing reaches the CAS or the metadata: the
    /// only writes are cache entries. Each file is keyed under the region
    /// node its own chunks produce ([`file_region_hash`]), the same hash a
    /// full commit of the unchanged file would name, so the entry is sound
    /// by construction - no committed tree is needed to confirm it, and the
    /// run works on an unborn store too.
    ///
    /// Unlike the commit-time pass, the image stays in hand from the read
    /// through the cache write: there is no metadata commit between them,
    /// so the bytes hashed are the bytes stored and no verifying re-read is
    /// needed. Memory stays bounded by the fan-out, one image per worker.
    ///
    /// The world stays stable for the read pass under a shared lock.
    /// `progress_builder` counts files as they complete. Hashing a corrupt
    /// or unsupported payload fails the run loudly, like ingest; a cache
    /// write failure fails it too, since updating the cache is the whole
    /// point of the command.
    ///
    /// [`file_region_hash`]: sekai_core::usecase::commit::file_region_hash
    pub async fn update_cache(
        &mut self,
        options: CacheUpdateOptions,
        progress_builder: &impl BuildProgress,
    ) -> Result<(CacheUpdateReport, CacheUpdateTimings), AppError> {
        let total = Instant::now();
        let guard = WorldGuard::acquire(self.world, WorldLockMode::Shared)?;
        let store = &mut self.instance.store;

        // Discovery walks directories on the blocking pool, owning a share
        // of the world lock like every file pass.
        let discover_started = Instant::now();
        let world_handle = self.world.clone();
        let discover_guard = Arc::clone(&guard);
        let regions = tokio::task::spawn_blocking(move || {
            let _guard = discover_guard;
            discover_regions(&world_handle)
        })
        .await??;
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
        // Workers clone the cache adapter over the same session; puts are
        // insert-if-absent, so concurrent writers never conflict.
        let adapter = store.cache().clone();
        let mut queued = regions.into_iter();
        let mut set = JoinSet::new();
        let spawn = |set: &mut JoinSet<Result<FileUpdate, AppError>>,
                     (key, region): (RegionKey, W::Region)| {
            let cache = adapter.clone();
            let guard = Arc::clone(&guard);
            set.spawn(async move { update_file(region, key, cache, guard).await });
        };
        for file in queued.by_ref().take(worker_slots(options.concurrency)) {
            spawn(&mut set, file);
        }
        // Completion order surfaces a failed file at once; returning drops
        // the set, which aborts the files still in flight. Entries already
        // written stay: the cache is append-only derived state.
        while let Some(joined) = set.join_next().await {
            let outcome = joined.map_err(|source| {
                AppError::Storage(StorageError::backend("cache update worker", source))
            })??;
            progress.step();
            report.chunks += outcome.chunks;
            if outcome.empty {
                report.empty += 1;
            } else if outcome.inserted {
                report.entries += 1;
            } else {
                report.existing += 1;
            }
            ingest += outcome.open + outcome.hash;
            hash += outcome.hash;
            cache += outcome.put;
            if let Some(file) = queued.next() {
                spawn(&mut set, file);
            }
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

/// One file's contribution to the update report and timings.
struct FileUpdate {
    /// Chunks hashed from the file.
    chunks: usize,
    /// The file holds no chunks, so it names no region node.
    empty: bool,
    /// The cache entry was newly inserted (vs. already present).
    inserted: bool,
    /// File open time.
    open: Duration,
    /// Chunk hashing time.
    hash: Duration,
    /// Cache write time.
    put: Duration,
}

/// Hash one file and store its image under the region node its chunks
/// produce. The read and hash run on the blocking pool holding `guard`;
/// the cache write then awaits with the captured bytes, no world access.
async fn update_file<R: RegionFile, C: CacheStore<Error = StorageError>, L: WorldLock>(
    region: R,
    key: RegionKey,
    mut cache: C,
    guard: Arc<WorldGuard<L>>,
) -> Result<FileUpdate, AppError>
where
    AppError: From<R::Error>,
{
    let hashed = tokio::task::spawn_blocking(move || {
        let _guard = guard;
        hash_region(&region, key)
    })
    .await
    .map_err(|source| AppError::Storage(StorageError::backend("cache update worker", source)))??;

    let (inserted, put) = if let Some(region_hash) = hashed.region {
        let started = Instant::now();
        let inserted = cache
            .put(region_hash, hashed.mca_hash, &hashed.image)
            .await?;
        (inserted, started.elapsed())
    } else {
        (false, Duration::ZERO)
    };
    Ok(FileUpdate {
        chunks: hashed.chunks,
        empty: hashed.region.is_none(),
        inserted,
        open: hashed.open,
        hash: hashed.hash,
        put,
    })
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
fn hash_region<R: RegionFile>(region: &R, key: RegionKey) -> Result<HashedRegion, AppError>
where
    AppError: From<R::Error>,
{
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
    let region = sekai_core::usecase::commit::file_region_hash(&entries);
    Ok(HashedRegion {
        region,
        mca_hash,
        image: image.image().to_vec(),
        chunks,
        open,
        hash,
    })
}
