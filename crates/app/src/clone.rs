//! Commit clone: rebuild region files into a fresh directory.
//!
//! Clone shares the checkout restore set (present rows from the
//! [`core`](sekai_core::usecase::checkout) plan) but never touches the
//! live world: every file lands under `out` at its layout-derived path,
//! so the destination must be missing or empty. Tombstones produce no
//! files; only vanilla namespaces are derivable (custom dimensions fail
//! loudly with the offending coordinates, as in checkout).

use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use sekai_core::{ChunkCoord, ChunkHash, CommitId, RegionKey, Scope};
use sekai_storage::{SekaiStore, StorageError};
use sekai_world::{RegionFile, WorldTree};
use tokio::task::{Id, JoinError, JoinSet};

use crate::checkout::{MissingBlobPolicy, fetch_or_skip};
use crate::error::AppError;
use crate::instance::SekaiInstance;
use crate::progress::{ProgressBuilder, Reporter};

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
    /// `scope` is cloned. `progress` fires as files complete, on the async
    /// side, counting them in the order their writes land, which need not
    /// be plan order. A failed or cancelled clone leaves `out` incomplete,
    /// but every file in it is whole.
    pub async fn clone_to<W: WorldTree>(
        &self,
        out: &W,
        commit: CommitId,
        options: CloneOptions,
        scope: Scope,
        progress: &impl ProgressBuilder,
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
            cas: store.blob().clone(),
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
/// Blob fetches run here, one file at a time; each file's write runs on the
/// blocking pool while the next file is fetched. A queued write holds a
/// whole region image, so at most one per CPU is in flight. Writes are
/// settled between files and at the end, in the order they land.
///
/// Returning early or being cancelled drops the writes that have not
/// started; those already running finish. Every write swaps a finished file
/// into place, so `out` may be incomplete but never holds a partial file.
async fn clone_files<S: SekaiStore, W: WorldTree>(
    job: CloneJob<S, W>,
    timestamp: u32,
    progress: &impl ProgressBuilder,
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
    let mut progress = progress.start(groups.len());
    let limit = std::thread::available_parallelism().map_or(4, NonZeroUsize::get);
    let mut writes = JoinSet::new();
    // A panicking write reaches us as a bare task id; this names its file.
    let mut files = HashMap::new();
    let mut blob_buf = Vec::new();
    for (key, rows) in &groups {
        let region = out.derive_region(*key)?;
        let mut writer = sekai_anvil::RegionBuilder::new(key.rx, key.rz, timestamp)?;
        let mut chunks = 0usize;
        for (coord, hash) in rows {
            if !fetch_or_skip(&cas, hash, options.on_missing_blob, &mut blob_buf).await? {
                continue;
            }
            writer.stage_chunk(coord.x, coord.z, &blob_buf)?;
            chunks += 1;
        }
        let image = writer.image()?;
        let write = writes.spawn_blocking(move || {
            region.write(&image).map_err(AppError::from)?;
            Ok(chunks)
        });
        files.insert(write.id(), *key);

        while let Some(landed) = writes.try_join_next_with_id() {
            settle(landed, &mut files, &mut report, &mut progress)?;
        }
        if writes.len() >= limit
            && let Some(landed) = writes.join_next_with_id().await
        {
            settle(landed, &mut files, &mut report, &mut progress)?;
        }
    }
    while let Some(landed) = writes.join_next_with_id().await {
        settle(landed, &mut files, &mut report, &mut progress)?;
    }
    Ok(report)
}

/// Count one landed write, or turn its failure into the clone's error.
fn settle(
    landed: Result<(Id, Result<usize, AppError>), JoinError>,
    files: &mut HashMap<Id, RegionKey>,
    report: &mut CloneReport,
    progress: &mut impl Reporter,
) -> Result<(), AppError> {
    let chunks = match landed {
        Ok((id, written)) => {
            files.remove(&id);
            written?
        }
        Err(err) => {
            let file = files.remove(&err.id()).map_or_else(
                || "a write".to_owned(),
                |key| format!("the write for {key:?}"),
            );
            return Err(AppError::Storage(StorageError::backend(
                "clone worker",
                std::io::Error::other(format!("{file} stopped: {err}")),
            )));
        }
    };
    report.files_written += 1;
    report.chunks_restored += chunks;
    progress.step(report.files_written);
    Ok(())
}
