//! Rollback execution: rebuild world files from a resolved snapshot plan.
//!
//! Rationale: the restore set (which blobs each region file needs) comes
//! from the [`core`](sekai_core::usecase::rollback) plan; what stays here
//! is mechanics `core` cannot own: world-layout discovery, path
//! derivation, and the atomic MCA rewrite itself. Region files are rebuilt
//! wholesale from CAS blobs (never patched), so on-disk chunks unknown to
//! the snapshot (created later) vanish along with post-snapshot regions,
//! and all-tombstone regions delete their file instead of leaving a
//! header-only shell. A blob missing from CAS aborts loudly: that is
//! corruption, and writing a partial world would be worse than writing none.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sekai_core::{BlobHash, ChunkCoord, RegionKey, RollbackReport, Scope, SnapshotId};
use sekai_storage::FileCas;
use sekai_world::LayoutFlavor;

use crate::error::AppError;

/// Per-phase timings for rollback execution.
#[derive(Debug, Clone, Default)]
pub struct RollbackTimings {
    /// Wall-clock total.
    pub total: Duration,
    /// Rollback plan resolution from metadata.
    pub plan: Duration,
    /// World flavor detection and region discovery.
    pub discover: Duration,
    /// Rebuilding MCA files, restoring chunks from CAS, and removing post-snapshot files.
    pub rollback_files: Duration,
}

/// Per-file progress report for the `progress` callback.
#[derive(Debug, Clone, Copy)]
pub struct RollbackProgress {
    /// Files fully rebuilt so far.
    pub files_done: usize,
    /// Region files to rebuild in total.
    pub files_total: usize,
    /// Chunks restored so far.
    pub chunks_done: usize,
}

/// Rebuild `world` from `snapshot` in the store at `store_url`, returning
/// execution report and per-phase timings.
///
/// Only `scope` is rebuilt: region files outside the scope are never
/// written, deleted, or otherwise touched. `progress` fires as files
/// complete; it must be `'static` because the file pass runs on a
/// blocking pool (pass a `move` closure owning its state).
pub async fn rollback(
    world: &Path,
    store_url: &str,
    snapshot: SnapshotId,
    scope: Scope,
    progress: impl Fn(RollbackProgress) + Send + 'static,
) -> Result<(RollbackReport, RollbackTimings), AppError> {
    let total_started = Instant::now();
    let store = super::open_store(store_url).await?;

    let plan_started = Instant::now();
    // Snapshot must exist; its timestamp stamps rebuilt files.
    let plan = sekai_core::usecase::rollback::plan_rollback(store.meta(), snapshot)
        .await
        .map_err(AppError::Rollback)?;
    let plan_dt = plan_started.elapsed();

    let timestamp = u32::try_from(plan.created_at_ms / 1000).unwrap_or(u32::MAX);

    // Scope before the blocking pass: out-of-scope regions are dropped
    // from both sides, so the file pass below cannot tell they exist.
    let mut groups = plan.groups;
    groups.retain(|key, _| scope.matches_region(*key));

    let discover_started = Instant::now();
    let flavor = sekai_world::detect_flavor(world)?;
    let world_buf = world.to_path_buf();
    let mut discovered: BTreeMap<RegionKey, PathBuf> = tokio::task::spawn_blocking(move || {
        sekai_world::discover(&world_buf).map(|regions| {
            regions
                .into_iter()
                .map(|r| {
                    (
                        RegionKey::new(r.dim, r.kind, r.region_x, r.region_z),
                        r.path,
                    )
                })
                .collect()
        })
    })
    .await??;
    let discover_dt = discover_started.elapsed();
    discovered.retain(|key, _| scope.matches_region(*key));

    let job = RollbackJob {
        groups,
        cas: store.cas().clone(),
        world: world.to_path_buf(),
        flavor,
        discovered,
    };

    let files_started = Instant::now();
    let report =
        tokio::task::spawn_blocking(move || rollback_files(job, timestamp, progress)).await??;
    let files_dt = files_started.elapsed();

    let timings = RollbackTimings {
        total: total_started.elapsed(),
        plan: plan_dt,
        discover: discover_dt,
        rollback_files: files_dt,
    };

    Ok((report, timings))
}

/// Everything one rollback file pass needs, owned for the blocking task.
struct RollbackJob {
    groups: BTreeMap<RegionKey, Vec<(ChunkCoord, BlobHash)>>,
    cas: FileCas,
    world: PathBuf,
    flavor: LayoutFlavor,
    discovered: BTreeMap<RegionKey, PathBuf>,
}

/// Rebuild every region file: present rows from CAS blobs, tombstones and
/// post-snapshot files deleted. Blocking: file reads, writes, and swaps
/// belong on a blocking pool, never on an async worker.
fn rollback_files(
    job: RollbackJob,
    timestamp: u32,
    progress: impl Fn(RollbackProgress),
) -> Result<RollbackReport, AppError> {
    let RollbackJob {
        groups,
        cas,
        world,
        flavor,
        discovered,
    } = job;
    let mut keys: BTreeSet<RegionKey> = discovered.keys().copied().collect();
    keys.extend(groups.keys().copied());

    let mut report = RollbackReport {
        files_written: 0,
        files_deleted: 0,
        chunks_restored: 0,
    };
    let files_total = keys.len();
    let mut files_done = 0usize;
    let mut progressed = |report: &RollbackReport| {
        files_done += 1;
        progress(RollbackProgress {
            files_done,
            files_total,
            chunks_done: report.chunks_restored,
        });
    };
    let mut blob_buf = Vec::new();
    for key in keys {
        let rows = groups.get(&key);
        let path = match discovered.get(&key) {
            Some(path) => path.clone(),
            None => match rows {
                None => continue, // No rows and no file: nothing to do.
                Some(_) => sibling_or_derived(&discovered, &world, &flavor, &key)?,
            },
        };
        let Some(rows) = rows else {
            // Strict rollback: the snapshot knows nothing of this file,
            // so it was created afterwards - remove it.
            if path.exists() {
                std::fs::remove_file(&path).map_err(|source| sekai_world::WorldError::Io {
                    path: path.clone(),
                    source,
                })?;
                report.files_deleted += 1;
            }
            progressed(&report);
            continue;
        };
        let mut writer = sekai_anvil::RegionBuilder::new(key.rx, key.rz, timestamp)?;
        for (coord, hash) in rows {
            // Missing blob = corruption: abort, do not write partial worlds.
            cas.fetch_blob(hash, &mut blob_buf)?;
            writer.stage_chunk(coord.x, coord.z, &blob_buf)?;
            report.chunks_restored += 1;
        }
        sekai_world::atomic_swap(&path, &writer.image()?)?;
        report.files_written += 1;
        progressed(&report);
    }
    Ok(report)
}

/// Target path for a snapshot-known region whose file is missing on disk.
///
/// Prefers a same-dimension sibling's directory (folders may have moved
/// since the backup, e.g. across a 26.1 migration); only derives from the
/// layout flavor when no sibling exists. Non-derivable namespaces fail
/// loudly instead of writing somewhere wrong.
fn sibling_or_derived(
    discovered: &BTreeMap<RegionKey, PathBuf>,
    world: &Path,
    flavor: &LayoutFlavor,
    key: &RegionKey,
) -> Result<PathBuf, AppError> {
    if let Some(sibling) = discovered
        .iter()
        .find(|(k, _)| k.dim == key.dim && k.kind == key.kind)
        .map(|(_, path)| path)
    {
        let mut path = sibling.clone();
        path.set_file_name(format!("r.{}.{}.mca", key.rx, key.rz));
        return Ok(path);
    }
    Ok(sekai_world::derive_path(
        world, flavor, key.dim, key.kind, key.rx, key.rz,
    )?)
}
