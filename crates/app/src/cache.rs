//! Best-effort region-level change proofs over the region-image cache
//! (ADR-0029, ADR-0030).
//!
//! A commit caches each fully covered file under the region node hash it
//! names, and only when the image it read produced exactly that hash. So
//! a live file whose bytes hash to the cached image digest holds exactly
//! the chunks the committed region node names: the whole region is proven
//! unchanged without decompressing or normalizing a single chunk.
//!
//! The proof is an optimization, never a decision: a cache miss, a byte
//! mismatch, or any read failure leaves the region unproven and the caller
//! falls back to its per-chunk path. Nothing here writes to the cache.

use std::collections::BTreeMap;
use std::sync::Arc;

use sekai_core::{CommitId, RegionHash, RegionKey, hash_bytes};
use sekai_storage::{CacheStore as _, MetaStore as _, RegionNode, SekaiStore};
use sekai_world::{Region, RegionFile, WorldLock};
use tokio::task::JoinSet;

use crate::instance::WorldGuard;
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
