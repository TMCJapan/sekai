//! Incremental world backup: fingerprint, ingest changes, carry the rest.
//!
//! Rationale: this is the composition root's execution half of backup. All
//! policy lives in [`core`](sekai_core::usecase::backup): the previous-state
//! plan (carry vs ingest), per-chunk hashing and staging, tombstone
//! assembly, and the atomic commit. What stays here is mechanics the core
//! cannot own without breaking its `no_std` boundary: world discovery and
//! fingerprint observation (filesystem), worker-thread ingest (threads),
//! the wall clock, and timing collection for `--timing` output.
//!
//! Crash order mirrors the core contract: every blob is flushed to CAS
//! *before* the single metadata batch commits, so a torn backup leaves at
//! most orphan blobs (reclaimed by future GC), never dangling references.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sekai_core::usecase::backup::{
    BackupReport, Observation, assemble, commit, hash_payload, plan_backup, stage_present,
};
use sekai_core::{
    ChunkCoord, RawChunk, RegionFingerprint, RegionKey, RegionReader as _, SnapshotEntry,
};
use sekai_mca::{RegionFile, RegionRef, discover, fingerprint_file};
use sekai_storage::{Blake3Hasher, FileCas, Store};

use crate::error::Error;
use crate::timing::{BackupTimings, RegionTiming};

/// Changed file plus its fresh fingerprint, awaiting ingest.
type Changed<'a> = (&'a RegionRef, RegionFingerprint);

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

/// Scan `world` and record it as a new snapshot in `store`.
pub fn backup(world: &Path, store: &mut Store) -> Result<BackupReport, Error> {
    Ok(backup_with_metrics(world, store)?.0)
}

/// Scan `world` like [`backup`], additionally returning per-phase timings.
///
/// The report is identical to [`backup`]'s; timings are informational only
/// and must never change backup semantics.
pub fn backup_with_metrics(
    world: &Path,
    store: &mut Store,
) -> Result<(BackupReport, BackupTimings), Error> {
    let total = Instant::now();

    let discover_started = Instant::now();
    let regions = discover(world)?;
    let discover = discover_started.elapsed();

    let fingerprint_started = Instant::now();
    let mut observed: Vec<Observation> = Vec::with_capacity(regions.len());
    for region in &regions {
        let key = RegionKey::new(region.dim, region.kind, region.region_x, region.region_z);
        observed.push(Observation {
            key,
            fingerprint: fingerprint_file(&region.path, key)?,
        });
    }
    let fingerprint = fingerprint_started.elapsed();

    let universe_started = Instant::now();
    let (previous, plan) = plan_backup(store.meta(), &observed)?;
    let universe_load = universe_started.elapsed();

    // Changed files in discovery order; carries need no file access.
    let mut files: HashMap<RegionKey, (&RegionRef, RegionFingerprint)> =
        HashMap::with_capacity(regions.len());
    for (region, obs) in regions.iter().zip(observed.iter()) {
        files.insert(obs.key, (region, obs.fingerprint));
    }
    let mut changed: Vec<Changed<'_>> = Vec::with_capacity(plan.ingest.len());
    for key in &plan.ingest {
        if let Some((region, fingerprint)) = files.remove(key) {
            changed.push((region, fingerprint));
        }
    }

    let mut walk = RegionWalk {
        entries: Vec::with_capacity(previous.universe.len()),
        present: HashSet::with_capacity(previous.universe.len()),
        new_blobs: 0,
        fingerprints: Vec::new(),
        region_open: Duration::ZERO,
        ingest: Duration::ZERO,
        hash: Duration::ZERO,
        cas: Duration::ZERO,
        timings: Vec::with_capacity(changed.len()),
    };
    ingest_changed(changed, store, &mut walk)?;

    let staged = assemble(
        plan,
        &previous,
        walk.entries,
        walk.present.into_iter().collect(),
        walk.new_blobs,
        walk.fingerprints,
    );

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))?;
    // Persist batched shard-directory renames before the metadata commit:
    // file data is already fsynced per blob, and this single barrier (at
    // most one fsync per touched shard) is what makes every referenced blob
    // crash-durable. Timed under `cas`, the durability bucket. Pinned to the
    // `BlobStore` seam (not the inherent method) so the call site only
    // depends on the port.
    let sync_started = Instant::now();
    sekai_core::BlobStore::sync(store.cas_mut())?;
    let dir_sync = sync_started.elapsed();
    let db_started = Instant::now();
    let report = commit(store.meta_mut(), &previous, &staged, now_ms)?;
    let db_apply = db_started.elapsed();

    let timings = BackupTimings {
        total: total.elapsed(),
        discover,
        universe_load,
        fingerprint,
        region_open: walk.region_open,
        ingest: walk.ingest,
        hash: walk.hash,
        cas_put: walk.cas + dir_sync,
        cas_checked: report.chunks,
        db_apply,
        skipped_regions: report.skipped_regions,
        carried_chunks: report.carried_chunks,
        regions: walk.timings,
    };
    Ok((report, timings))
}

/// Ingest changed files, sequentially or across worker threads.
///
/// One file never justifies thread overhead, so it stays on the calling
/// thread with identical counts. Larger sets split into size-balanced
/// groups; each worker owns a private `FileCas` handle over the same root
/// (disjoint blob files, unique temp names) and syncs its own shards before
/// returning, so the scope join orders every durability barrier ahead of the
/// metadata commit. The commit itself stays a single transaction; row merge
/// order across workers is completion-order (DB-irrelevant, one txn).
fn ingest_changed(
    changed: Vec<Changed<'_>>,
    store: &mut Store,
    walk: &mut RegionWalk,
) -> Result<(), Error> {
    if changed.len() <= 1 {
        for (region, observed) in changed {
            let mut entries = Vec::new();
            let mut present = HashSet::new();
            let mut new_blobs = 0usize;
            let (fingerprint, timing) = ingest_file(
                region,
                observed,
                store.cas_mut(),
                &mut entries,
                &mut present,
                &mut new_blobs,
            )?;
            merge_outcome(
                walk,
                FileOutcome {
                    fingerprint,
                    entries,
                    present,
                    new_blobs,
                    timing,
                },
            );
        }
        return Ok(());
    }
    // Owned root: workers must not borrow `store` (its SQLite handle is not
    // shareable across threads), only the CAS directory path.
    let cas_root = store.cas().root().to_path_buf();
    let groups = partition_groups(changed);
    let count = groups.len();
    let (tx, rx) = mpsc::channel();
    thread::scope(|scope| {
        for group in groups {
            let tx = tx.clone();
            let cas_root = cas_root.clone();
            scope.spawn(move || {
                let outcome = ingest_group(group, &cas_root);
                // The receiver outlives the scope, so this fails only if the
                // worker panicked; library code has no panic paths, and a
                // missing message still surfaces below via the receive count.
                let _ = tx.send(outcome);
            });
        }
    });
    drop(tx);
    for _ in 0..count {
        match rx.recv() {
            Ok(Ok((outcomes, sync_elapsed))) => {
                walk.cas += sync_elapsed;
                for outcome in outcomes {
                    merge_outcome(walk, outcome);
                }
            }
            Ok(Err(err)) => return Err(err),
            Err(_) => {
                return Err(Error::Io {
                    path: cas_root,
                    source: std::io::Error::other("cas worker ended without reporting"),
                });
            }
        }
    }
    // Completion order is nondeterministic; keep timing output stable.
    walk.timings.sort_by_key(|timing| timing.path.clone());
    Ok(())
}

/// Split changed files into size-balanced groups, one per worker.
///
/// Largest files first onto the currently lightest group; worker count is
/// hardware parallelism bounded by actual work, so every group is non-empty.
fn partition_groups<'a>(mut changed: Vec<Changed<'a>>) -> Vec<Vec<Changed<'a>>> {
    changed.sort_by_key(|item| std::cmp::Reverse(item.1.size));
    let workers = thread::available_parallelism()
        .map_or(4, NonZeroUsize::get)
        .min(changed.len());
    let mut groups: Vec<Vec<Changed<'a>>> = Vec::with_capacity(workers);
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
    group: Vec<Changed<'_>>,
    cas_root: &Path,
) -> Result<(Vec<FileOutcome>, Duration), Error> {
    let mut cas = FileCas::open(cas_root)?;
    let mut outcomes = Vec::with_capacity(group.len());
    for (region, observed) in group {
        let mut entries = Vec::new();
        let mut present = HashSet::new();
        let mut new_blobs = 0usize;
        let (fingerprint, timing) = ingest_file(
            region,
            observed,
            &mut cas,
            &mut entries,
            &mut present,
            &mut new_blobs,
        )?;
        outcomes.push(FileOutcome {
            fingerprint,
            entries,
            present,
            new_blobs,
            timing,
        });
    }
    // Barrier for this worker's shards; the scope join orders all barriers
    // before the metadata commit. Pinned to the port seam.
    let sync_started = Instant::now();
    sekai_core::BlobStore::sync(&mut cas)?;
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

/// Read, hash, and store one changed region file, staging its rows.
///
/// Row staging goes through the out-params so the sequential and worker
/// paths share one shape; the returned fingerprint feeds the derived-state
/// upsert and the timing slice feeds `--timing`.
fn ingest_file(
    region: &RegionRef,
    observed: RegionFingerprint,
    cas: &mut FileCas,
    entries: &mut Vec<SnapshotEntry>,
    present: &mut HashSet<ChunkCoord>,
    new_blobs: &mut usize,
) -> Result<(RegionFingerprint, RegionTiming), Error> {
    let opened = Instant::now();
    let file = RegionFile::open(&region.path, region.dim, region.kind)?;
    let open_elapsed = opened.elapsed();
    let bytes = file.image().len() as u64;

    let mut failure: Option<Error> = None;
    let mut chunks = 0usize;
    let mut hash_elapsed = Duration::ZERO;
    let mut cas_elapsed = Duration::ZERO;
    let ingested = Instant::now();
    file.visit_chunks(|chunk| {
        if failure.is_some() {
            return false;
        }
        match ingest_timed(chunk, cas, entries, present, new_blobs) {
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
    })?;
    let ingest_elapsed = ingested.elapsed();
    if let Some(err) = failure {
        return Err(err);
    }
    Ok((
        observed,
        RegionTiming {
            path: region.path.clone(),
            bytes,
            chunks,
            open: open_elapsed,
            ingest: ingest_elapsed,
            hash: hash_elapsed,
            cas: cas_elapsed,
        },
    ))
}

/// Hash one raw chunk, flush it to CAS, and stage its history row.
///
/// Hashing and staging go through the core use-case helpers so the digest
/// construction and the row shape have a single owner; only the clock reads
/// around them live here for `--timing`. Returns the time spent hashing and
/// the time spent in `CAS put` separately so digest and exists-check/write
/// costs stay visible in [`BackupTimings`].
fn ingest_timed(
    chunk: RawChunk<'_>,
    cas: &mut FileCas,
    entries: &mut Vec<SnapshotEntry>,
    present: &mut HashSet<ChunkCoord>,
    new_blobs: &mut usize,
) -> Result<(Duration, Duration), Error> {
    let hash_started = Instant::now();
    let hash = hash_payload::<Blake3Hasher>(chunk.payload);
    let hash_elapsed = hash_started.elapsed();
    let cas_started = Instant::now();
    // Pinned to the `BlobStore` seam (not the inherent method) so the call
    // site only depends on the port.
    if sekai_core::BlobStore::put(cas, &hash, chunk.payload)? {
        *new_blobs += 1;
    }
    let cas_elapsed = cas_started.elapsed();
    entries.push(stage_present(chunk.coord, hash));
    present.insert(chunk.coord);
    Ok((hash_elapsed, cas_elapsed))
}
