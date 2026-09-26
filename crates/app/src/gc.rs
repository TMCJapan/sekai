//! Garbage collection over store snapshots and CAS blobs.

use std::time::{Duration, Instant};

use sekai_core::{GcPlan, GcReport};

use crate::error::AppError;

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

/// Inspect store and return a plan of unreferenced orphan blobs,
/// additionally returning per-phase timings (`apply` is zero: dry-run
/// never unlinks).
pub async fn gc_plan(store_url: &str) -> Result<(GcPlan, GcTimings), AppError> {
    let total_started = Instant::now();
    let store = super::open_store(store_url).await?;
    let (plan, plan_dt) = plan_within(&store).await?;
    let timings = GcTimings {
        total: total_started.elapsed(),
        plan: plan_dt,
        apply: Duration::ZERO,
    };
    Ok((plan, timings))
}

/// Execute a garbage collection plan, removing orphan blobs.
/// `progress` fires per examined orphan candidate.
pub async fn gc_apply(
    store_url: &str,
    plan: &GcPlan,
    progress: impl FnMut(GcProgress) + Send,
) -> Result<(GcReport, GcTimings), AppError> {
    let total_started = Instant::now();
    let mut store = super::open_store(store_url).await?;
    let (report, apply_dt) = apply_within(&mut store, plan, progress).await?;
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
    store_url: &str,
    progress: impl FnMut(GcProgress) + Send,
) -> Result<(GcReport, GcTimings), AppError> {
    let total_started = Instant::now();
    let mut store = super::open_store(store_url).await?;
    let (plan, plan_dt) = plan_within(&store).await?;
    let (report, apply_dt) = apply_within(&mut store, &plan, progress).await?;
    let timings = GcTimings {
        total: total_started.elapsed(),
        plan: plan_dt,
        apply: apply_dt,
    };
    Ok((report, timings))
}

/// Build the orphan plan against an open store, refusing a busy one.
async fn plan_within(store: &sekai_storage::SqliteStore) -> Result<(GcPlan, Duration), AppError> {
    store.cas().ensure_idle()?;
    let started = Instant::now();
    let plan = sekai_core::usecase::gc::gc_plan(store.cas(), store.meta())
        .await
        .map_err(AppError::Gc)?;
    Ok((plan, started.elapsed()))
}

/// Unlink `plan`'s orphans against an open store.
///
/// The busy check repeats here rather than trusting the planning pass: a
/// backup that started in between would have put blobs into the CAS that
/// this very plan calls orphaned.
async fn apply_within(
    store: &mut sekai_storage::SqliteStore,
    plan: &GcPlan,
    mut progress: impl FnMut(GcProgress) + Send,
) -> Result<(GcReport, Duration), AppError> {
    store.cas().ensure_idle()?;
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
