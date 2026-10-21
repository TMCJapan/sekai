//! Garbage collection over store snapshots and CAS blobs.

use std::time::{Duration, Instant};

use sekai_core::{GcPlan, GcReport};
use sekai_storage::{SekaiStore, StoreLockMode};

use crate::error::AppError;
use crate::instance::SekaiInstance;

/// Per-phase timings for garbage collection.
#[derive(Debug, Clone, Default)]
pub struct GcTimings {
    /// Wall-clock total.
    pub total: Duration,
    /// Scanning metadata and CAS blobs to build the candidate orphan plan.
    pub plan: Duration,
    /// Re-verifying and unlinking orphan blobs from storage.
    pub apply: Duration,
}

/// Per-blob progress report for the `progress` callback.
#[derive(Debug, Clone, Copy)]
pub struct GcProgress {
    /// Orphan candidates examined so far.
    pub blobs_done: usize,
    /// Orphan candidates in total.
    pub blobs_total: usize,
}

impl<S: SekaiStore> SekaiInstance<S> {
    /// Inspect the store and return a plan of unreferenced orphan blobs,
    /// additionally returning per-phase timings (`apply` is zero: dry-run
    /// never unlinks).
    ///
    /// Refuses while a backup runs: its blobs reach the CAS before the rows
    /// that reference them, so the plan would list them as orphans.
    pub async fn gc_plan(&self) -> Result<(GcPlan, GcTimings), AppError> {
        let _lock = self.lock_store(StoreLockMode::Writer)?;
        let total_started = Instant::now();
        let (plan, plan_dt) = plan_within(self.store()).await?;
        let timings = GcTimings {
            total: total_started.elapsed(),
            plan: plan_dt,
            apply: Duration::ZERO,
        };
        Ok((plan, timings))
    }

    /// Execute a garbage collection plan, removing orphan blobs.
    /// `progress` fires per examined orphan candidate.
    ///
    /// Each candidate is re-verified against the store under this call's
    /// lock, so a plan from an earlier [`Self::gc_plan`] never unlinks a
    /// blob that a backup has referenced since; blobs orphaned after
    /// planning wait for the next run. [`Self::gc`] does both under one
    /// lock.
    pub async fn gc_apply(
        &mut self,
        plan: &GcPlan,
        progress: impl FnMut(GcProgress) + Send,
    ) -> Result<(GcReport, GcTimings), AppError> {
        let _lock = self.lock_store(StoreLockMode::Writer)?;
        let total_started = Instant::now();
        let (report, apply_dt) = apply_within(self.store_mut(), plan, progress).await?;
        let timings = GcTimings {
            total: total_started.elapsed(),
            plan: Duration::ZERO,
            apply: apply_dt,
        };
        Ok((report, timings))
    }

    /// Run garbage collection plan and apply in a single pass.
    /// `progress` fires per examined orphan candidate during apply.
    pub async fn gc(
        &mut self,
        progress: impl FnMut(GcProgress) + Send,
    ) -> Result<(GcReport, GcTimings), AppError> {
        // One lock across both phases: a backup slipping in between would
        // put blobs into the CAS that this very plan calls orphaned.
        let _lock = self.lock_store(StoreLockMode::Writer)?;
        let total_started = Instant::now();
        let (plan, plan_dt) = plan_within(self.store()).await?;
        let (report, apply_dt) = apply_within(self.store_mut(), &plan, progress).await?;
        let timings = GcTimings {
            total: total_started.elapsed(),
            plan: plan_dt,
            apply: apply_dt,
        };
        Ok((report, timings))
    }
}

/// Build the orphan plan against an open store.
async fn plan_within<S: SekaiStore>(store: &S) -> Result<(GcPlan, Duration), AppError> {
    let started = Instant::now();
    let plan = sekai_core::usecase::gc::gc_plan(store.cas(), store.meta())
        .await
        .map_err(AppError::Gc)?;
    Ok((plan, started.elapsed()))
}

/// Unlink `plan`'s orphans against an open store.
async fn apply_within<S: SekaiStore>(
    store: &mut S,
    plan: &GcPlan,
    mut progress: impl FnMut(GcProgress) + Send,
) -> Result<(GcReport, Duration), AppError> {
    let started = Instant::now();
    let (cas, meta) = store.cas_and_meta();
    let report = sekai_core::usecase::gc::gc_apply(cas, meta, plan, |done, total| {
        progress(GcProgress {
            blobs_done: done,
            blobs_total: total,
        });
    })
    .await
    .map_err(AppError::Gc)?;
    Ok((report, started.elapsed()))
}
