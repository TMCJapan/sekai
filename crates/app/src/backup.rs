//! Incremental world backup: fingerprint, ingest changes, carry the rest.
//!
//! Rationale: this is the orchestration half of backup. All policy lives in
//! [`core`](sekai_core::usecase::backup): the previous-state plan (carry vs
//! ingest), per-chunk staging, tombstone assembly, and the atomic commit.
//! What stays here is mechanics `core` cannot own without breaking its
//! `no_std` boundary: world discovery and fingerprint observation
//! (filesystem), worker ingest on a blocking pool (threads + fsync), the
//! wall clock, and timing collection for `--timing` output.
//!
//! Crash order mirrors the core contract: every blob is flushed to CAS
//! *before* the single metadata batch commits, so a torn backup leaves at
//! most orphan blobs (reclaimed by future GC), never dangling references.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sekai_core::{
    BackupReport, ChunkCoord, Observation, RegionFingerprint, RegionKey, SnapshotEntry,
};
use sekai_storage::FileCas;
use sekai_world::RegionRef;

use crate::error::AppError;

/// Backup behavior knobs.
#[derive(Debug, Clone, Default)]
pub struct BackupOptions {
    /// Ingest worker count. `0` means one per CPU, `1` stays sequential.
    pub concurrency: usize,
    /// Derive volatile diff views alongside blobs. Decode failures degrade
    /// to "not computed" (`None`): the blob itself is intact, and the diff
    /// column is only a cache.
    pub with_diff: bool,
    /// Tag names ignored by diff views. `None` selects the default set.
    pub ignore_tags: Option<Vec<String>>,
}

/// Per-file progress report for the `progress` callback.
#[derive(Debug, Clone, Copy)]
pub struct BackupProgress {
    /// Files fully ingested so far.
    pub files_done: usize,
    /// Changed files to ingest in total.
    pub files_total: usize,
    /// Chunks ingested so far.
    pub chunks_done: usize,
}

/// Per-phase timings. Informational only; never changes backup semantics.
#[derive(Debug, Clone, Default)]
pub struct BackupTimings {
    /// Wall-clock total.
    pub total: Duration,
    /// World discovery.
    pub discover: Duration,
    /// Fingerprint observation.
    pub fingerprint: Duration,
    /// Previous-state load plus carry/ingest planning.
    pub universe_load: Duration,
    /// Opening changed region files.
    pub region_open: Duration,
    /// Visiting, hashing, and storing changed chunks.
    pub ingest: Duration,
    /// Subset of `ingest` spent hashing (plus diffing when enabled).
    pub hash: Duration,
    /// Subset of `ingest` spent in `CAS put`, plus the durability barrier.
    pub cas_put: Duration,
    /// Metadata commit.
    pub db_apply: Duration,
    /// Fingerprint-matched regions carried over.
    pub skipped_regions: usize,
    /// History rows carried over.
    pub carried_chunks: usize,
    /// Per-region details for ingested files, in discovery order.
    pub regions: Vec<RegionTiming>,
}

/// Per-region timing slice.
#[derive(Debug, Clone)]
pub struct RegionTiming {
    /// Region file path.
    pub path: PathBuf,
    /// File size in bytes.
    pub bytes: u64,
    /// Chunks ingested from the file.
    pub chunks: usize,
    /// File open time.
    pub open: Duration,
    /// Visit/hash/store time.
    pub ingest: Duration,
    /// Subset of `ingest` spent hashing.
    pub hash: Duration,
    /// Subset of `ingest` spent in `CAS put`.
    pub cas: Duration,
}

/// Changed file plus its fresh fingerprint, awaiting ingest.
type Changed = (RegionRef, RegionFingerprint);

/// One ingested file's staged rows, ready to merge into the walk.
struct FileOutcome {
    /// Fresh fingerprint for the derived-state upsert.
    fingerprint: RegionFingerprint,
    /// History rows for the file's chunks.
    entries: Vec<SnapshotEntry>,
    /// Coordinates present in the file.
    present: HashSet<ChunkCoord>,
    /// Blobs newly written to CAS.
    new_blobs: usize,
    /// Per-region timing slice.
    timing: RegionTiming,
}

/// Scan `world` and record it as a new snapshot in the store at
/// `store_url`, additionally returning per-phase timings.
///
/// `progress` fires as changed files complete. The report is identical
/// with or without timings.
pub async fn backup(
    world: &Path,
    store_url: &str,
    options: BackupOptions,
    progress: impl Fn(BackupProgress) + Send,
) -> Result<(BackupReport, BackupTimings), AppError> {
    let total = Instant::now();
    let mut store = super::open_store(store_url).await?;

    let observed = tokio::task::spawn_blocking({
        let world = world.to_path_buf();
        move || observe(&world)
    })
    .await??;
    let discover = observed.discover;
    let fingerprint = observed.fingerprint;

    let universe_started = Instant::now();
    let (previous, plan) =
        sekai_core::usecase::backup::plan_backup(store.meta(), &observed.observations).await?;
    let universe_load = universe_started.elapsed();

    let mut files: HashMap<RegionKey, Changed> = HashMap::with_capacity(observed.regions.len());
    for (region, obs) in observed
        .regions
        .into_iter()
        .zip(observed.observations.iter())
    {
        files.insert(obs.key, (region, obs.fingerprint));
    }
    // Changed files in discovery order; carries need no file access.
    let mut changed: Vec<Changed> = Vec::with_capacity(plan.ingest.len());
    for key in &plan.ingest {
        if let Some(entry) = files.remove(key) {
            changed.push(entry);
        }
    }

    let walk = ingest_changed(changed, store.cas().root(), &options, progress).await?;

    let staged = sekai_core::usecase::backup::assemble(
        plan,
        &previous,
        walk.entries,
        walk.present.into_iter().collect(),
        walk.new_blobs,
        walk.fingerprints,
    );

    let now_ms = now_ms()?;
    // Persist batched shard-directory renames before the metadata commit:
    // file data is already synced per blob, and this single barrier (at
    // most one fsync per touched shard) is what makes every referenced blob
    // crash-durable. Timed under `cas`, the durability bucket. Explicit
    // here even though workers already synced their own handles: the
    // barrier belongs at the call site, never hidden inside adapters.
    let sync_started = Instant::now();
    store.cas_mut().sync_dirs()?;
    let dir_sync = sync_started.elapsed();
    let db_started = Instant::now();
    let report =
        sekai_core::usecase::backup::commit(store.meta_mut(), &previous, &staged, now_ms).await?;
    let db_apply = db_started.elapsed();

    let timings = BackupTimings {
        total: total.elapsed(),
        discover,
        fingerprint,
        universe_load,
        region_open: walk.region_open,
        ingest: walk.ingest,
        hash: walk.hash,
        cas_put: walk.cas + dir_sync,
        db_apply,
        skipped_regions: report.skipped_regions,
        carried_chunks: report.carried_chunks,
        regions: walk.timings,
    };
    Ok((report, timings))
}

/// Whole-world observation: discovery plus fresh fingerprints.
struct Observed {
    regions: Vec<sekai_world::RegionRef>,
    observations: Vec<Observation>,
    discover: Duration,
    fingerprint: Duration,
}

/// Discover every region file and fingerprint it. Blocking: file walks and
/// opens belong on a blocking pool, never on an async worker.
fn observe(world: &Path) -> Result<Observed, AppError> {
    let discover_started = Instant::now();
    let regions = sekai_world::discover(world)?;
    let discover = discover_started.elapsed();

    let fingerprint_started = Instant::now();
    let mut observed: Vec<Observation> = Vec::with_capacity(regions.len());
    for region in &regions {
        let key = RegionKey::new(region.dim, region.kind, region.region_x, region.region_z);
        observed.push(Observation {
            key,
            fingerprint: sekai_world::fingerprint_file(&region.path, key)?,
        });
    }
    Ok(Observed {
        regions,
        observations: observed,
        discover,
        fingerprint: fingerprint_started.elapsed(),
    })
}

/// Staged result of walking all discovered regions.
struct RegionWalk {
    /// Fresh history rows (ingested chunks; tombstones join at assembly).
    entries: Vec<SnapshotEntry>,
    /// Coordinates present on disk.
    present: HashSet<ChunkCoord>,
    /// Blobs newly written to CAS.
    new_blobs: usize,
    /// Fresh fingerprints for ingested files.
    fingerprints: Vec<RegionFingerprint>,
    /// Time spent opening changed region files.
    region_open: Duration,
    /// Time spent visiting, hashing, and storing changed chunks.
    ingest: Duration,
    /// Subset of `ingest` spent hashing.
    hash: Duration,
    /// Subset of `ingest` spent in `CAS put`.
    cas: Duration,
    /// Per-region details for ingested files, in discovery order.
    timings: Vec<RegionTiming>,
}

/// Ingest changed files, sequentially or across worker threads.
///
/// One file never justifies task overhead, so it stays in a single blocking
/// task with identical counts. Larger sets split into size-balanced groups;
/// each worker owns a private `FileCas` handle over the same root (disjoint
/// blob files, unique temp names) and syncs its own shards before
/// returning, so joining orders every durability barrier ahead of the
/// metadata commit. The commit itself stays a single transaction; row merge
/// order across workers is completion-order (DB-irrelevant, one txn).
async fn ingest_changed(
    changed: Vec<Changed>,
    cas_root: &Path,
    options: &BackupOptions,
    progress: impl Fn(BackupProgress) + Send,
) -> Result<RegionWalk, AppError> {
    let mut walk = RegionWalk {
        entries: Vec::new(),
        present: HashSet::new(),
        new_blobs: 0,
        fingerprints: Vec::new(),
        region_open: Duration::ZERO,
        ingest: Duration::ZERO,
        hash: Duration::ZERO,
        cas: Duration::ZERO,
        timings: Vec::with_capacity(changed.len()),
    };
    if changed.is_empty() {
        return Ok(walk);
    }
    let workers = match options.concurrency {
        0 => std::thread::available_parallelism().map_or(4, NonZeroUsize::get),
        n => n,
    }
    .min(changed.len())
    .max(1);
    let groups = partition_groups(changed, workers);
    let files_total: usize = groups.iter().map(Vec::len).sum();
    let mut files_done = 0usize;
    let mut chunks_done = 0usize;

    let mut set = tokio::task::JoinSet::new();
    for group in groups {
        let cas_root = cas_root.to_path_buf();
        let with_diff = options.with_diff;
        let ignore_tags = options.ignore_tags.clone();
        set.spawn_blocking(move || {
            ingest_group(group, &cas_root, with_diff, ignore_tags.as_deref())
        });
    }
    while let Some(outcome) = set.join_next().await {
        let (outcomes, sync_elapsed) = outcome??;
        walk.cas += sync_elapsed;
        for outcome in outcomes {
            files_done += 1;
            chunks_done += outcome.timing.chunks;
            progress(BackupProgress {
                files_done,
                files_total,
                chunks_done,
            });
            merge_outcome(&mut walk, outcome);
        }
    }
    // Completion order is nondeterministic; keep timing output stable.
    walk.timings.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(walk)
}

/// Split changed files into size-balanced groups, one per worker.
///
/// Largest files first onto the currently lightest group, so every group
/// is non-empty.
fn partition_groups(mut changed: Vec<Changed>, workers: usize) -> Vec<Vec<Changed>> {
    changed.sort_by_key(|item| std::cmp::Reverse(item.1.size));
    let mut groups: Vec<Vec<Changed>> = Vec::with_capacity(workers);
    groups.resize_with(workers, Vec::new);
    let mut loads = vec![0u64; workers];
    for item in changed {
        let mut lightest = 0usize;
        for (index, load) in loads.iter().enumerate() {
            if *load < loads[lightest] {
                lightest = index;
            }
        }
        loads[lightest] += item.1.size;
        groups[lightest].push(item);
    }
    groups.retain(|group| !group.is_empty());
    groups
}

/// Ingest one worker's group with a private CAS handle, then sync its shards.
///
/// Returns the per-file outcomes plus the shard-sync time so the caller can
/// account it under `cas` (the durability bucket).
fn ingest_group(
    group: Vec<Changed>,
    cas_root: &Path,
    with_diff: bool,
    ignore_tags: Option<&[String]>,
) -> Result<(Vec<FileOutcome>, Duration), AppError> {
    let mut cas = FileCas::open(cas_root)?;
    let mut outcomes = Vec::with_capacity(group.len());
    for (region, observed) in group {
        let outcome = ingest_file(&region, observed, &mut cas, with_diff, ignore_tags)?;
        outcomes.push(outcome);
    }
    // Barrier for this worker's shards; the join orders all barriers
    // before the metadata commit.
    let sync_started = Instant::now();
    cas.sync_dirs()?;
    Ok((outcomes, sync_started.elapsed()))
}

/// Fold one file's staged rows into the walk.
fn merge_outcome(walk: &mut RegionWalk, outcome: FileOutcome) {
    walk.entries.extend(outcome.entries);
    walk.present.extend(outcome.present);
    walk.new_blobs += outcome.new_blobs;
    walk.region_open += outcome.timing.open;
    walk.ingest += outcome.timing.ingest;
    walk.hash += outcome.timing.hash;
    walk.cas += outcome.timing.cas;
    walk.fingerprints.push(outcome.fingerprint);
    walk.timings.push(outcome.timing);
}

struct ChunkIngestCtx<'a> {
    region: &'a RegionRef,
    cas: &'a mut FileCas,
    scratch: &'a mut Vec<u8>,
    with_diff: bool,
    ignore: &'a [&'a str],
    entries: &'a mut Vec<SnapshotEntry>,
    present: &'a mut HashSet<ChunkCoord>,
    new_blobs: &'a mut usize,
}

/// Read, hash, and store one changed region file, staging its rows.
fn ingest_file(
    region: &RegionRef,
    observed: RegionFingerprint,
    cas: &mut FileCas,
    with_diff: bool,
    ignore_tags: Option<&[String]>,
) -> Result<FileOutcome, AppError> {
    let opened = Instant::now();
    let bytes = sekai_world::open_image(&region.path)?;
    let open_elapsed = opened.elapsed();
    let file_bytes = bytes.len() as u64;
    let image = sekai_anvil::RegionImage::from_bytes(bytes, region.region_x, region.region_z)
        .map_err(|source| AppError::RegionFailed {
            path: region.path.clone(),
            source,
        })?;

    let mut ignore_vec = Vec::new();
    let ignore: &[&str] = ignore_tags.map_or(sekai_nbt::DEFAULT_IGNORED, |tags| {
        ignore_vec = tags.iter().map(String::as_str).collect();
        &ignore_vec
    });

    let mut scratch = Vec::new();
    let mut entries = Vec::new();
    let mut present = HashSet::new();
    let mut new_blobs = 0usize;
    let mut failure: Option<AppError> = None;
    let mut chunks = 0usize;
    let mut hash_elapsed = Duration::ZERO;
    let mut cas_elapsed = Duration::ZERO;
    let ingested = Instant::now();
    image
        .visit_chunks(|chunk| {
            if failure.is_some() {
                return false;
            }
            let mut ctx = ChunkIngestCtx {
                region,
                cas,
                scratch: &mut scratch,
                with_diff,
                ignore,
                entries: &mut entries,
                present: &mut present,
                new_blobs: &mut new_blobs,
            };
            match ingest_timed(chunk, &mut ctx) {
                Ok((hash_dt, cas_dt)) => {
                    hash_elapsed += hash_dt;
                    cas_elapsed += cas_dt;
                    chunks += 1;
                }
                Err(err) => {
                    failure = Some(err);
                    return false;
                }
            }
            true
        })
        .map_err(|source| AppError::RegionFailed {
            path: region.path.clone(),
            source,
        })?;
    let ingest_elapsed = ingested.elapsed();
    if let Some(err) = failure {
        return Err(err);
    }
    Ok(FileOutcome {
        fingerprint: observed,
        entries,
        present,
        new_blobs,
        timing: RegionTiming {
            path: region.path.clone(),
            bytes: file_bytes,
            chunks,
            open: open_elapsed,
            ingest: ingest_elapsed,
            hash: hash_elapsed,
            cas: cas_elapsed,
        },
    })
}

/// Hash one raw chunk, flush it to CAS, and stage its history row.
///
/// Returns the time spent hashing (plus diffing when enabled) and the time
/// spent in `CAS put` separately so digest and exists-check/write costs
/// stay visible in [`BackupTimings`].
fn ingest_timed(
    chunk: sekai_anvil::Chunk<'_>,
    ctx: &mut ChunkIngestCtx<'_>,
) -> Result<(Duration, Duration), AppError> {
    let coord = ChunkCoord::new(ctx.region.dim, ctx.region.kind, chunk.x, chunk.z);
    let hash_started = Instant::now();
    let hash = sekai_core::hash_blob(chunk.payload);
    // Diff failures degrade to "not computed": the blob itself is intact
    // and the diff column is only a cache.
    let diff = ctx.with_diff.then(|| {
        sekai_anvil::decompress_into(chunk.payload, ctx.scratch)
            .ok()
            .and_then(|_| sekai_core::diff_hash(ctx.scratch, ctx.ignore).ok())
    });
    let hash_elapsed = hash_started.elapsed();
    let cas_started = Instant::now();
    if ctx.cas.put_blob(&hash, chunk.payload)? {
        *ctx.new_blobs += 1;
    }
    let cas_elapsed = cas_started.elapsed();
    ctx.entries
        .push(SnapshotEntry::new(coord, Some(hash), diff.flatten()));
    ctx.present.insert(coord);
    Ok((hash_elapsed, cas_elapsed))
}

/// Wall-clock now as Unix millis.
fn now_ms() -> Result<u64, AppError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(AppError::Clock)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
