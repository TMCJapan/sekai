//! Rollback execution: rebuild world files from a resolved snapshot plan.
//!
//! Rationale: the restore set (which blobs each region file needs) comes
//! from the [`core`](sekai_core::usecase::rollback) plan; what stays here
//! is mechanics `core` cannot own: world-layout discovery, path
//! derivation, and the atomic MCA rewrite itself. Region files are rebuilt
//! wholesale from CAS blobs (never patched). Under the default
//! [`RollbackOptions`] on-disk chunks unknown to the snapshot (created
//! later) vanish along with post-snapshot regions, all-tombstone regions
//! delete their file instead of leaving a header-only shell, and a blob
//! missing from CAS aborts loudly: that is corruption, and writing a
//! partial world would be worse than writing none. Keep-policies in
//! [`RollbackOptions`] relax each of those strict behaviors explicitly.

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

/// What to do when a snapshot blob is absent from CAS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MissingBlobPolicy {
    /// Abort loudly: a missing blob is corruption, and writing a partial
    /// world would be worse than writing none.
    #[default]
    Abort,
    /// Skip the chunk, leaving it out of the rebuilt file.
    SkipChunk,
}

/// Where to rebuild a snapshot-known region whose file is missing on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MissingFilePolicy {
    /// Prefer a same-dimension sibling's directory, falling back to the
    /// layout-derived path.
    #[default]
    SiblingFirst,
    /// Always use the layout-derived path, ignoring siblings.
    DerivedOnly,
    /// Fail loudly instead of guessing a location.
    Error,
}

/// Rollback restore policy. `Default` is strict: snapshot-unknown files
/// are deleted, tombstoned chunks vanish, missing blobs abort, and
/// missing files resolve sibling-first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RollbackOptions {
    /// Keep region files unknown to the snapshot instead of deleting them.
    pub keep_post_snapshot_files: bool,
    /// Keep live bytes for chunks unknown to the snapshot (created
    /// afterwards inside a snapshot-known region) instead of dropping them.
    pub keep_post_snapshot_chunks: bool,
    /// Keep live bytes for snapshot-tombstoned chunks instead of removing
    /// them. Fully tombstoned region files are left alone rather than
    /// deleted.
    pub keep_tombstoned_chunks: bool,
    /// How to handle a snapshot blob missing from CAS.
    pub on_missing_blob: MissingBlobPolicy,
    /// Where to rebuild a snapshot-known region whose file is missing.
    pub on_missing_file: MissingFilePolicy,
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
    options: RollbackOptions,
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
    let mut tombstones = plan.tombstones;
    tombstones.retain(|key, _| scope.matches_region(*key));

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
        tombstones,
        options,
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
    tombstones: BTreeMap<RegionKey, Vec<ChunkCoord>>,
    options: RollbackOptions,
    cas: FileCas,
    world: PathBuf,
    flavor: LayoutFlavor,
    discovered: BTreeMap<RegionKey, PathBuf>,
}

/// Rebuild every region file: present rows from CAS blobs, tombstones and
/// post-snapshot files deleted unless kept by [`RollbackOptions`].
/// Blocking: file reads, writes, and swaps belong on a blocking pool,
/// never on an async worker.
fn rollback_files(
    job: RollbackJob,
    timestamp: u32,
    progress: impl Fn(RollbackProgress),
) -> Result<RollbackReport, AppError> {
    let RollbackJob {
        groups,
        tombstones,
        options,
        cas,
        world,
        flavor,
        discovered,
    } = job;
    let mut keys: BTreeSet<RegionKey> = discovered.keys().copied().collect();
    keys.extend(groups.keys().copied());
    keys.extend(tombstones.keys().copied());

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
        let tombs = tombstones.get(&key);
        let path = match discovered.get(&key) {
            Some(path) => path.clone(),
            None => match rows {
                None => continue, // No rows and no file: nothing to do.
                Some(_) => {
                    resolve_target(&discovered, &world, &flavor, &key, options.on_missing_file)?
                }
            },
        };
        let Some(rows) = rows else {
            // No present rows: either fully tombstoned at the snapshot or
            // created afterwards. Keep-policies leave the live file alone;
            // strict rollback deletes it.
            let keep = if tombs.is_some() {
                options.keep_tombstoned_chunks
            } else {
                options.keep_post_snapshot_files
            };
            if !keep && path.exists() {
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
        let mut restored: BTreeSet<(i32, i32)> = BTreeSet::new();
        for (coord, hash) in rows {
            match cas.fetch_blob(hash, &mut blob_buf) {
                Ok(()) => {}
                Err(sekai_storage::StorageError::BlobMissing { .. })
                    if options.on_missing_blob == MissingBlobPolicy::SkipChunk =>
                {
                    continue;
                }
                Err(source) => return Err(source.into()),
            }
            writer.stage_chunk(coord.x, coord.z, &blob_buf)?;
            restored.insert((coord.x, coord.z));
            report.chunks_restored += 1;
        }
        if options.keep_post_snapshot_chunks || options.keep_tombstoned_chunks {
            merge_live_chunks(&path, &key, &restored, tombs, options, &mut writer)?;
        }
        sekai_world::atomic_swap(&path, &writer.image()?)?;
        report.files_written += 1;
        progressed(&report);
    }
    Ok(report)
}

/// Merge live on-disk chunks that the snapshot does not restore into
/// `writer`: tombstoned coordinates under `keep_tombstoned_chunks`,
/// snapshot-unknown ones under `keep_post_snapshot_chunks`. Missing live
/// files contribute nothing; corrupt ones fail loudly.
#[allow(clippy::too_many_arguments)]
fn merge_live_chunks(
    path: &Path,
    key: &RegionKey,
    restored: &BTreeSet<(i32, i32)>,
    tombs: Option<&Vec<ChunkCoord>>,
    options: RollbackOptions,
    writer: &mut sekai_anvil::RegionBuilder,
) -> Result<(), AppError> {
    if !path.exists() {
        return Ok(());
    }
    let bytes = std::fs::read(path).map_err(|source| sekai_world::WorldError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let image = sekai_anvil::RegionImage::from_bytes(bytes, key.rx, key.rz).map_err(|source| {
        AppError::RegionFailed {
            path: path.to_path_buf(),
            source,
        }
    })?;
    let tombed: BTreeSet<(i32, i32)> = tombs
        .map(|coords| coords.iter().map(|c| (c.x, c.z)).collect())
        .unwrap_or_default();
    let mut failed = None;
    image
        .visit_chunks(|chunk| {
            if restored.contains(&(chunk.x, chunk.z)) {
                return true;
            }
            let keep = if tombed.contains(&(chunk.x, chunk.z)) {
                options.keep_tombstoned_chunks
            } else {
                options.keep_post_snapshot_chunks
            };
            if keep && let Err(source) = writer.stage_chunk(chunk.x, chunk.z, chunk.payload) {
                failed = Some(source);
                return false;
            }
            true
        })
        .map_err(|source| AppError::RegionFailed {
            path: path.to_path_buf(),
            source,
        })?;
    if let Some(source) = failed {
        return Err(AppError::Anvil(source));
    }
    Ok(())
}

/// Target path for a snapshot-known region whose file is missing on disk,
/// per [`MissingFilePolicy`].
fn resolve_target(
    discovered: &BTreeMap<RegionKey, PathBuf>,
    world: &Path,
    flavor: &LayoutFlavor,
    key: &RegionKey,
    policy: MissingFilePolicy,
) -> Result<PathBuf, AppError> {
    match policy {
        MissingFilePolicy::Error => Err(sekai_world::WorldError::UnknownRegionPath {
            dim: key.dim.raw(),
            kind: key.kind.raw(),
            region_x: key.rx,
            region_z: key.rz,
        }
        .into()),
        MissingFilePolicy::DerivedOnly => Ok(sekai_world::derive_path(
            world, flavor, key.dim, key.kind, key.rx, key.rz,
        )?),
        MissingFilePolicy::SiblingFirst => sibling_or_derived(discovered, world, flavor, key),
    }
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
