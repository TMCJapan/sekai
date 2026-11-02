//! Commit clone: rebuild region files into a fresh directory.
//!
//! Clone shares the checkout restore set (present rows from the
//! [`core`](sekai_core::usecase::checkout) plan) but never touches the
//! live world: every file lands under `out` at its layout-derived path,
//! so the destination must be missing or empty. Tombstones produce no
//! files; only vanilla namespaces are derivable (custom dimensions fail
//! loudly with the offending coordinates, as in checkout).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use sekai_core::{ChunkCoord, ChunkHash, CommitId, RegionKey, Scope};
use sekai_storage::{SekaiStore, StorageError};
use sekai_world::{RegionFile, WorldTree};

use crate::checkout::{MissingBlobPolicy, fetch_or_skip};
use crate::error::AppError;
use crate::instance::SekaiInstance;

/// Clone policy. `Default` aborts on missing blobs, as in checkout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CloneOptions {
    /// How to handle a commit blob missing from CAS.
    pub on_missing_blob: MissingBlobPolicy,
}

/// Outcome of one clone run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloneReport {
    /// Region files written.
    pub files_written: usize,
    /// Chunks restored from CAS blobs.
    pub chunks_restored: usize,
}

/// Per-phase timings for clone execution.
#[derive(Debug, Clone, Default)]
pub struct CloneTimings {
    /// Wall-clock total.
    pub total: Duration,
    /// Clone plan resolution from metadata.
    pub plan: Duration,
    /// Rebuilding MCA files from CAS blobs.
    pub clone_files: Duration,
}

/// Per-file progress report for the `progress` callback.
#[derive(Debug, Clone, Copy)]
pub struct CloneProgress {
    /// Files fully rebuilt so far.
    pub files_done: usize,
    /// Files to rebuild in total.
    pub files_total: usize,
    /// Chunks restored so far.
    pub chunks_done: usize,
}

impl<S: SekaiStore> SekaiInstance<S> {
    /// Rebuild `commit` from the store into `out`, returning execution
    /// report and per-phase timings.
    ///
    /// `out` is created when missing and must otherwise be empty; anything
    /// else fails loudly so clone can never clobber existing data. Nothing
    /// locks `out`, so it must be dedicated to this clone: a writer that
    /// starts there after the emptiness check is not excluded. Its
    /// layout comes from how it was constructed (for example
    /// `HostWorldTree::new_legacy` or `HostWorldTree::new_bukkit`). Only
    /// `scope` is cloned. `progress` fires as files complete; it must be
    /// `'static` because the file pass runs on a blocking pool (pass a
    /// `move` closure owning its state).
    pub async fn clone_to<W: WorldTree>(
        &self,
        out: &W,
        commit: CommitId,
        options: CloneOptions,
        scope: Scope,
        progress: impl Fn(CloneProgress) + Send + 'static,
    ) -> Result<(CloneReport, CloneTimings), AppError>
    where
        AppError: From<W::Error>,
    {
        // `out` stays unlocked: it is new and empty by contract, so nothing
        // else has a reason to touch it. The store needs no lock either:
        // blob reads and the metadata reads that resolved the plan run
        // side by side with concurrent commits.
        let total_started = Instant::now();

        if !out.is_empty()? {
            return Err(std::io::Error::new(
                std::io::ErrorKind::DirectoryNotEmpty,
                "clone target world is not empty",
            )
            .into());
        }

        let store = self.store();

        let plan_started = Instant::now();
        let plan = sekai_core::usecase::checkout::plan_checkout(store.meta(), commit)
            .await
            .map_err(AppError::Checkout)?;
        let plan_dt = plan_started.elapsed();

        let timestamp = u32::try_from(plan.created_at_ms / 1000).unwrap_or(u32::MAX);

        let mut groups = plan.groups;
        groups.retain(|key, _| scope.matches_region(*key));

        let job = CloneJob::<S, W> {
            groups,
            options,
            cas: store.cas().clone(),
            out: out.clone(),
        };

        let files_started = Instant::now();
        let report = clone_files(job, timestamp, progress).await;
        let files_dt = files_started.elapsed();
        let report = report?;

        let timings = CloneTimings {
            total: total_started.elapsed(),
            plan: plan_dt,
            clone_files: files_dt,
        };

        Ok((report, timings))
    }
}

/// Everything one clone file pass needs.
struct CloneJob<S: SekaiStore, W: WorldTree> {
    groups: BTreeMap<RegionKey, Vec<(ChunkCoord, ChunkHash)>>,
    options: CloneOptions,
    cas: S::Blobs,
    out: W,
}

/// Rebuild every commit region file under `out`.
///
/// Blob fetches are async; each file's build and write runs on the blocking
/// pool, never on an async worker.
async fn clone_files<S: SekaiStore, W: WorldTree>(
    job: CloneJob<S, W>,
    timestamp: u32,
    progress: impl Fn(CloneProgress),
) -> Result<CloneReport, AppError>
where
    AppError: From<W::Error>,
{
    let CloneJob {
        groups,
        options,
        cas,
        out,
    } = job;

    let mut report = CloneReport {
        files_written: 0,
        chunks_restored: 0,
    };
    let files_total = groups.len();
    let mut files_done = 0usize;
    let mut blob_buf = Vec::new();
    let mut steps = Vec::new();
    for (key, rows) in &groups {
        let region = out.derive_region(*key)?;
        let mut writer = sekai_anvil::RegionBuilder::new(key.rx, key.rz, timestamp)?;
        for (coord, hash) in rows {
            if !fetch_or_skip(&cas, hash, options.on_missing_blob, &mut blob_buf).await? {
                continue;
            }
            writer.stage_chunk(coord.x, coord.z, &blob_buf)?;
            report.chunks_restored += 1;
        }
        // Queued as soon as its input is ready: a cancelled future must not
        // take a file it has already promised to write with it.
        let image = writer.image()?;
        steps.push((
            tokio::task::spawn_blocking(move || region.write(&image).map_err(AppError::from)),
            key,
        ));
    }
    // Collect in plan order, so a failure names the file it came from.
    for (step, key) in steps {
        step.await.map_err(|err| {
            AppError::Storage(StorageError::backend(
                "clone worker",
                std::io::Error::other(format!("the file step for {key:?} stopped: {err}")),
            ))
        })??;
        report.files_written += 1;
        files_done += 1;
        progress(CloneProgress {
            files_done,
            files_total,
            chunks_done: report.chunks_restored,
        });
    }
    Ok(report)
}
