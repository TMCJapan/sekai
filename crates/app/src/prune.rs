//! Snapshot pruning over store metadata.
//!
//! Planning is read-only; applying folds each deleted snapshot into its
//! next retained one atomically. Blobs are never unlinked here: pruning
//! dereferences them, and a later GC reclaims the orphans.

use std::time::{Duration, Instant};

use sekai_core::{PrunePlan, PruneReport, SnapshotId};

use crate::error::AppError;
use crate::instance::SekaiInstance;

/// Per-phase timings for snapshot pruning.
#[derive(Debug, Clone, Default)]
pub struct PruneTimings {
    /// Wall-clock total.
    pub total: Duration,
    /// Selecting the retained set and listing deletions.
    pub plan: Duration,
    /// Folding and deleting snapshots.
    pub apply: Duration,
}

/// Per-snapshot progress report for the `progress` callback.
#[derive(Debug, Clone, Copy)]
pub struct PruneProgress {
    /// Snapshots deleted so far.
    pub snapshots_done: usize,
    /// Snapshots to delete in total.
    pub snapshots_total: usize,
}

impl SekaiInstance {
    /// Inspect snapshots and return a deletion plan.
    ///
    /// Retention keeps `keep_last` newest intersected with `before`-and-newer.
    /// Additionally returns per-phase timings (`apply` is zero: dry-run never
    /// deletes).
    pub async fn prune_plan(
        &self,
        keep_last: Option<u64>,
        before: Option<SnapshotId>,
    ) -> Result<(PrunePlan, PruneTimings), AppError> {
        let total_started = Instant::now();
        let (plan, plan_dt) = plan_within(self.store(), keep_last, before).await?;
        let timings = PruneTimings {
            total: total_started.elapsed(),
            plan: plan_dt,
            apply: Duration::ZERO,
        };
        Ok((plan, timings))
    }

    /// Execute a pruning plan, deleting snapshots oldest-first.
    /// `progress` fires per deleted snapshot.
    pub async fn prune_apply(
        &mut self,
        plan: &PrunePlan,
        progress: impl FnMut(PruneProgress) + Send,
    ) -> Result<(PruneReport, PruneTimings), AppError> {
        let total_started = Instant::now();
        let (report, apply_dt) = apply_within(self.store_mut(), plan, progress).await?;
        let timings = PruneTimings {
            total: total_started.elapsed(),
            plan: Duration::ZERO,
            apply: apply_dt,
        };
        Ok((report, timings))
    }

    /// Plan and apply snapshot pruning in a single pass.
    /// `progress` fires per deleted snapshot during apply.
    pub async fn prune(
        &mut self,
        keep_last: Option<u64>,
        before: Option<SnapshotId>,
        progress: impl FnMut(PruneProgress) + Send,
    ) -> Result<(PruneReport, PruneTimings), AppError> {
        let total_started = Instant::now();
        let (plan, plan_dt) = plan_within(self.store(), keep_last, before).await?;
        let (report, apply_dt) = apply_within(self.store_mut(), &plan, progress).await?;
        let timings = PruneTimings {
            total: total_started.elapsed(),
            plan: plan_dt,
            apply: apply_dt,
        };
        Ok((report, timings))
    }
}

/// Select the retained set and list the deletions an open store would make.
async fn plan_within(
    store: &sekai_storage::SqliteStore,
    keep_last: Option<u64>,
    before: Option<SnapshotId>,
) -> Result<(PrunePlan, Duration), AppError> {
    let started = Instant::now();
    let snapshots = sekai_core::usecase::snapshot::list_snapshots(store.meta())
        .await
        .map_err(AppError::Snapshot)?;
    let retained = sekai_core::usecase::prune::select_retained(&snapshots, keep_last, before);
    let plan = sekai_core::usecase::prune::prune_plan(store.meta(), &retained)
        .await
        .map_err(AppError::Prune)?;
    Ok((plan, started.elapsed()))
}

/// Apply `plan` against an open store, reporting the phase duration.
async fn apply_within(
    store: &mut sekai_storage::SqliteStore,
    plan: &PrunePlan,
    mut progress: impl FnMut(PruneProgress) + Send,
) -> Result<(PruneReport, Duration), AppError> {
    let started = Instant::now();
    let report = sekai_core::usecase::prune::prune_apply(store.meta_mut(), plan, |done, total| {
        progress(PruneProgress {
            snapshots_done: done,
            snapshots_total: total,
        });
    })
    .await
    .map_err(AppError::Prune)?;
    Ok((report, started.elapsed()))
}
