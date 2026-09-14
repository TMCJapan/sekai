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

/// Inspect store and return a plan of unreferenced orphan blobs.
pub async fn gc_plan(store_url: &str) -> Result<GcPlan, AppError> {
    let store = super::open_store(store_url).await?;
    let plan = sekai_core::usecase::gc::gc_plan(store.cas(), store.meta())
        .await
        .map_err(AppError::Gc)?;
    Ok(plan)
}

/// Execute a garbage collection plan, removing orphan blobs.
pub async fn gc_apply(store_url: &str, plan: &GcPlan) -> Result<(GcReport, GcTimings), AppError> {
    let total_started = Instant::now();
    let mut store = super::open_store(store_url).await?;

    let apply_started = Instant::now();
    let (cas, meta) = store.cas_and_meta();
    let report = sekai_core::usecase::gc::gc_apply(cas, meta, plan)
        .await
        .map_err(AppError::Gc)?;
    let apply_dt = apply_started.elapsed();

    let timings = GcTimings {
        total: total_started.elapsed(),
        plan: Duration::ZERO,
        apply: apply_dt,
    };

    Ok((report, timings))
}

/// Run garbage collection plan and apply in a single pass.
pub async fn gc(store_url: &str) -> Result<(GcReport, GcTimings), AppError> {
    let total_started = Instant::now();
    let mut store = super::open_store(store_url).await?;

    let plan_started = Instant::now();
    let plan = sekai_core::usecase::gc::gc_plan(store.cas(), store.meta())
        .await
        .map_err(AppError::Gc)?;
    let plan_dt = plan_started.elapsed();

    let apply_started = Instant::now();
    let (cas, meta) = store.cas_and_meta();
    let report = sekai_core::usecase::gc::gc_apply(cas, meta, &plan)
        .await
        .map_err(AppError::Gc)?;
    let apply_dt = apply_started.elapsed();

    let timings = GcTimings {
        total: total_started.elapsed(),
        plan: plan_dt,
        apply: apply_dt,
    };

    Ok((report, timings))
}
