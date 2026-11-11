//! Commit clone: rebuild region files into a fresh directory.
//!
//! Clone shares the checkout restore set (present rows from the
//! [`core`](sekai_core::usecase::checkout) plan) but never touches the
//! live world: every file lands under `out` at its layout-derived path,
//! so the destination must be missing or empty. Tombstones produce no
//! files; only vanilla namespaces are derivable (custom dimensions fail
//! loudly with the offending coordinates, as in checkout).

use std::any::Any;
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use sekai_core::{ChunkCoord, ChunkHash, CommitId, RegionKey, Scope};
use sekai_storage::{SekaiStore, StorageError};
use sekai_world::{RegionFile, WorldTree};

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
    /// be plan order.
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

/// One file's write, reported back from the blocking pool once it ends.
struct LandedWrite {
    /// Chunks the file holds, counted only if the write succeeded.
    chunks: usize,
    outcome: Result<(), AppError>,
}

/// Rebuild every commit region file under `out`.
///
/// Blob fetches are async; each file's build and write runs on the blocking
/// pool, never on an async worker.
///
/// Landed writes are drained once per queued file, so a write that lands or
/// fails while the next file is fetched is reported only after that file is
/// queued: a failure can let one more file reach `out`, and progress holds
/// still for one file's fetch.
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
    let mut blob_buf = Vec::new();
    // Each write reports back as it lands, so nothing waits on an older
    // write and no handle has to be kept around.
    let (landed_tx, mut landed_rx) = tokio::sync::mpsc::unbounded_channel::<LandedWrite>();
    let mut spawned = 0usize;
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
        let key = *key;
        let landed_tx = landed_tx.clone();
        // Spawned as soon as its input is ready and detached at once: a
        // cancelled future must not take a file it has already promised to
        // write with it.
        tokio::task::spawn_blocking(move || {
            let outcome = catch_unwind(AssertUnwindSafe(|| region.write(&image)))
                .map_err(|payload| {
                    clone_worker_error(format!(
                        "the write for {key:?} panicked: {}",
                        panic_message(&*payload)
                    ))
                })
                .and_then(|written| written.map_err(AppError::from));
            // The receiver is gone only once the clone is abandoned.
            let _ = landed_tx.send(LandedWrite { chunks, outcome });
        });
        spawned += 1;
        while let Ok(landed) = landed_rx.try_recv() {
            settle(&mut report, landed, &mut progress)?;
        }
    }
    drop(landed_tx);
    while report.files_written < spawned {
        // Every sender gone with writes unreported means the blocking pool
        // dropped a write unrun (runtime shutdown).
        let landed = landed_rx.recv().await.ok_or_else(|| {
            clone_worker_error("a write stopped before reporting back".to_owned())
        })?;
        settle(&mut report, landed, &mut progress)?;
    }
    Ok(report)
}

fn settle(
    report: &mut CloneReport,
    landed: LandedWrite,
    progress: &mut impl Reporter,
) -> Result<(), AppError> {
    landed.outcome?;
    report.files_written += 1;
    report.chunks_restored += landed.chunks;
    progress.step(report.files_written);
    Ok(())
}

fn clone_worker_error(detail: String) -> AppError {
    AppError::Storage(StorageError::backend(
        "clone worker",
        std::io::Error::other(detail),
    ))
}

/// The message a panic was raised with: `panic!` carries a `&str` or a
/// `String`; any other payload has nothing printable.
fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

#[cfg(test)]
mod tests {
    use super::panic_message;
    use std::panic::catch_unwind;

    #[test]
    fn panic_message_reads_str_and_string_payloads() {
        let literal = catch_unwind(|| panic!("disk on fire")).unwrap_err();
        assert_eq!(panic_message(&*literal), "disk on fire");

        let formatted = catch_unwind(|| panic!("region {}", 7)).unwrap_err();
        assert_eq!(panic_message(&*formatted), "region 7");

        let other = catch_unwind(|| std::panic::panic_any(7_u8)).unwrap_err();
        assert_eq!(panic_message(&*other), "non-string panic payload");
    }
}
