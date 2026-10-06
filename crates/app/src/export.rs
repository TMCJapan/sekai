//! Snapshot export: rebuild region files into a fresh directory.
//!
//! Export shares the rollback restore set (present rows from the
//! [`core`](sekai_core::usecase::rollback) plan) but never touches the
//! live world: every file lands under `out` at its layout-derived path,
//! so the destination must be missing or empty. Tombstones produce no
//! files; only vanilla namespaces are derivable (custom dimensions fail
//! loudly with the offending coordinates, as in rollback).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use sekai_core::{BlobHash, ChunkCoord, RegionKey, Scope, SnapshotId};
use sekai_storage::FileCas;
use sekai_world::WorldTree;

use crate::error::AppError;
use crate::instance::SekaiInstance;
use crate::rollback::{MissingBlobPolicy, fetch_or_skip};

/// Export policy. `Default` aborts on missing blobs, as in rollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExportOptions {
    /// How to handle a snapshot blob missing from CAS.
    pub on_missing_blob: MissingBlobPolicy,
}

/// Outcome of one export run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportReport {
    /// Region files written.
    pub files_written: usize,
    /// Chunks restored from CAS blobs.
    pub chunks_restored: usize,
}

/// Per-phase timings for export execution.
#[derive(Debug, Clone, Default)]
pub struct ExportTimings {
    /// Wall-clock total.
    pub total: Duration,
    /// Export plan resolution from metadata.
    pub plan: Duration,
    /// Rebuilding MCA files from CAS blobs.
    pub export_files: Duration,
}

/// Per-file progress report for the `progress` callback.
#[derive(Debug, Clone, Copy)]
pub struct ExportProgress {
    /// Files fully rebuilt so far.
    pub files_done: usize,
    /// Region files to rebuild in total.
    pub files_total: usize,
    /// Chunks restored so far.
    pub chunks_done: usize,
}

impl SekaiInstance {
    /// Rebuild `snapshot` from the store into `out`, returning execution
    /// report and per-phase timings.
    ///
    /// `out` is created when missing and must otherwise be empty; anything
    /// else fails loudly so export can never clobber existing data. Its
    /// layout comes from how it was constructed (for example
    /// `HostWorldTree::new_legacy` or `HostWorldTree::new_bukkit`). Only
    /// `scope` is exported. `progress` fires as files complete; it must be
    /// `'static` because the file pass runs on a blocking pool (pass a
    /// `move` closure owning its state).
    pub async fn export(
        &self,
        out: &impl WorldTree,
        snapshot: SnapshotId,
        options: ExportOptions,
        scope: Scope,
        progress: impl Fn(ExportProgress) + Send + 'static,
    ) -> Result<(ExportReport, ExportTimings), AppError> {
        let total_started = Instant::now();

        if !out.is_empty()? {
            return Err(std::io::Error::new(
                std::io::ErrorKind::DirectoryNotEmpty,
                "export target world is not empty",
            )
            .into());
        }

        let store = self.store();

        let plan_started = Instant::now();
        let plan = sekai_core::usecase::rollback::plan_rollback(store.meta(), snapshot)
            .await
            .map_err(AppError::Rollback)?;
        let plan_dt = plan_started.elapsed();

        let timestamp = u32::try_from(plan.created_at_ms / 1000).unwrap_or(u32::MAX);

        let mut groups = plan.groups;
        groups.retain(|key, _| scope.matches_region(*key));

        let job = ExportJob {
            groups,
            options,
            cas: store.cas().clone(),
            out: out.clone(),
        };

        let files_started = Instant::now();
        let report =
            tokio::task::spawn_blocking(move || export_files(job, timestamp, progress)).await??;
        let files_dt = files_started.elapsed();

        let timings = ExportTimings {
            total: total_started.elapsed(),
            plan: plan_dt,
            export_files: files_dt,
        };

        Ok((report, timings))
    }
}

/// Everything one export file pass needs, owned for the blocking task.
struct ExportJob<T: WorldTree> {
    groups: BTreeMap<RegionKey, Vec<(ChunkCoord, BlobHash)>>,
    options: ExportOptions,
    cas: FileCas,
    out: T,
}

/// Rebuild every snapshot region file under `out`. Blocking: file reads,
/// writes, and swaps belong on a blocking pool, never on an async worker.
fn export_files(
    job: ExportJob<impl WorldTree>,
    timestamp: u32,
    progress: impl Fn(ExportProgress),
) -> Result<ExportReport, AppError> {
    let ExportJob {
        groups,
        options,
        cas,
        out,
    } = job;

    let mut report = ExportReport {
        files_written: 0,
        chunks_restored: 0,
    };
    let files_total = groups.len();
    let mut files_done = 0usize;
    let mut blob_buf = Vec::new();
    for (key, rows) in &groups {
        let path = out.derive_path(key.dim, key.kind, key.rx, key.rz)?;
        let mut writer = sekai_anvil::RegionBuilder::new(key.rx, key.rz, timestamp)?;
        for (coord, hash) in rows {
            if !fetch_or_skip(&cas, hash, options.on_missing_blob, &mut blob_buf)? {
                continue;
            }
            writer.stage_chunk(coord.x, coord.z, &blob_buf)?;
            report.chunks_restored += 1;
        }
        sekai_world::atomic_swap(&path, &writer.image()?)?;
        report.files_written += 1;
        files_done += 1;
        progress(ExportProgress {
            files_done,
            files_total,
            chunks_done: report.chunks_restored,
        });
    }
    Ok(report)
}
