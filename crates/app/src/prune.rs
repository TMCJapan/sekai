//! Commit pruning over store metadata.
//!
//! Planning is read-only; applying folds each deleted commit into its
//! next retained one atomically. Blobs are never unlinked here: pruning
//! dereferences them, and a later GC reclaims the orphans.

use std::time::{Duration, Instant};

use sekai_core::{CommitId, PrunePlan, PruneReport};
use sekai_storage::{SekaiStore, StoreLockMode};

use crate::error::AppError;
use crate::instance::SekaiInstance;

/// Per-phase timings for commit pruning.
#[derive(Debug, Clone, Default)]
pub struct PruneTimings {
    /// Wall-clock total.
    pub total: Duration,
    /// Selecting the retained set and listing deletions.
    pub plan: Duration,
    /// Folding and deleting commits.
    pub apply: Duration,
}

/// Per-commit progress report for the `progress` callback.
#[derive(Debug, Clone, Copy)]
pub struct PruneProgress {
    /// Commits deleted so far.
    pub commits_done: usize,
    /// Commits to delete in total.
    pub commits_total: usize,
}

impl<S: SekaiStore> SekaiInstance<S> {
    /// Inspect commits and return a deletion plan.
    ///
    /// Retention keeps `keep_last` newest intersected with `before`-and-newer.
    /// Additionally returns per-phase timings (`apply` is zero: dry-run never
    /// deletes).
    pub async fn prune_plan(
        &self,
        keep_last: Option<u64>,
        before: Option<CommitId>,
    ) -> Result<(PrunePlan, PruneTimings), AppError> {
        let _lock = self.lock_store(StoreLockMode::Shared)?;
        let total_started = Instant::now();
        let (plan, plan_dt) = plan_within(self.store(), keep_last, before).await?;
        let timings = PruneTimings {
            total: total_started.elapsed(),
            plan: plan_dt,
            apply: Duration::ZERO,
        };
        Ok((plan, timings))
    }

    /// Execute a pruning plan, deleting commits oldest-first.
    /// `progress` fires per deleted commit.
    ///
    /// A plan from an earlier [`Self::prune_plan`] call stays applicable
    /// after commits, which only add newer commits. If another prune
    /// retired one of its commits first, applying fails with an
    /// unknown-commit error rather than folding into the wrong one.
    /// [`Self::prune`] plans and applies under one lock.
    pub async fn prune_apply(
        &mut self,
        plan: &PrunePlan,
        progress: impl FnMut(PruneProgress) + Send,
    ) -> Result<(PruneReport, PruneTimings), AppError> {
        // Exclusive: deleted rows may be the ones a concurrent reader has
        // just resolved.
        let _lock = self.lock_store(StoreLockMode::Exclusive)?;
        let total_started = Instant::now();
        let (report, apply_dt) = apply_within(self.store_mut(), plan, progress).await?;
        let timings = PruneTimings {
            total: total_started.elapsed(),
            plan: Duration::ZERO,
            apply: apply_dt,
        };
        Ok((report, timings))
    }

    /// Plan and apply commit pruning in a single pass.
    /// `progress` fires per deleted commit during apply.
    pub async fn prune(
        &mut self,
        keep_last: Option<u64>,
        before: Option<CommitId>,
        progress: impl FnMut(PruneProgress) + Send,
    ) -> Result<(PruneReport, PruneTimings), AppError> {
        let _lock = self.lock_store(StoreLockMode::Exclusive)?;
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
async fn plan_within<S: SekaiStore>(
    store: &S,
    keep_last: Option<u64>,
    before: Option<CommitId>,
) -> Result<(PrunePlan, Duration), AppError> {
    let started = Instant::now();
    let commits = sekai_core::usecase::graph::list_commits(store.meta())
        .await
        .map_err(AppError::Commit)?;
    let retained = sekai_core::usecase::prune::select_retained(&commits, keep_last, before);
    let plan = sekai_core::usecase::prune::prune_plan(store.meta(), &retained)
        .await
        .map_err(AppError::Prune)?;
    Ok((plan, started.elapsed()))
}

/// Apply `plan` against an open store, reporting the phase duration.
async fn apply_within<S: SekaiStore>(
    store: &mut S,
    plan: &PrunePlan,
    mut progress: impl FnMut(PruneProgress) + Send,
) -> Result<(PruneReport, Duration), AppError> {
    let started = Instant::now();
    let report = sekai_core::usecase::prune::prune_apply(store.meta_mut(), plan, |done, total| {
        progress(PruneProgress {
            commits_done: done,
            commits_total: total,
        });
    })
    .await
    .map_err(AppError::Prune)?;
    Ok((report, started.elapsed()))
}
