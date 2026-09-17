//! Snapshot pruning subcommand execution, DTOs, and output rendering.

use anyhow::Context as _;
use sekai_app::{PrunePlan, PruneReport, PruneTimings};
use serde::Serialize;

use super::ReportOut;
use super::progress::{finish_progress, progress_bar, report_progress};
use crate::envelope::envelope_ok;
use crate::style::Styler;

pub async fn run(
    store: &str,
    keep_last: Option<u64>,
    before: Option<String>,
    dry_run: bool,
    progress: bool,
    out: ReportOut,
) -> anyhow::Result<()> {
    let style = out.style;
    let before = match before {
        Some(raw) => Some(
            sekai_app::resolve_snapshot_ref(store, &raw)
                .await
                .with_context(|| format!("snapshot {raw:?} failed to resolve"))?,
        ),
        None => None,
    };
    if dry_run {
        let (plan, timings) = sekai_app::prune_plan(store, keep_last, before)
            .await
            .with_context(|| format!("prune plan for {store} failed"))?;
        if out.json {
            println!(
                "{}",
                envelope_ok("prune", &prune_plan_payload(&plan, &timings, out.timing))?
            );
            return Ok(());
        }
        println!(
            "prune plan created: {} snapshots to delete ({})",
            highlight_count(style, plan.delete.len()),
            ids(&plan.delete),
        );
        if out.timing {
            print_prune_timing_table(&timings, style);
        }
        return Ok(());
    }

    let bar = progress_bar(progress);
    let (report, timings) = sekai_app::prune(store, keep_last, before, |update| {
        report_progress(
            bar.as_ref(),
            update.snapshots_done,
            update.snapshots_total,
            "snapshots",
        );
    })
    .await
    .with_context(|| format!("prune for {store} failed"))?;
    finish_progress(bar.as_ref());
    if out.json {
        println!(
            "{}",
            envelope_ok("prune", &prune_payload(&report, &timings, out.timing))?
        );
        return Ok(());
    }
    println!(
        "prune completed: {} deleted, {} rows folded, {} rows dropped (run gc to reclaim blobs)",
        highlight_count(style, report.pruned),
        report.rows_folded,
        report.rows_dropped
    );
    if out.timing {
        print_prune_timing_table(&timings, style);
    }
    Ok(())
}

fn ids(ids: &[sekai_app::SnapshotId]) -> String {
    ids.iter()
        .map(|id| id.raw().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Serialize)]
struct PrunePayload {
    pruned: usize,
    rows_folded: usize,
    rows_dropped: usize,
    #[serde(flatten)]
    timing: Option<PruneTiming>,
}

#[derive(Serialize)]
struct PruneTiming {
    total_ms: u128,
    phases: PrunePhases,
}

#[allow(clippy::struct_field_names)]
#[derive(Serialize)]
struct PrunePhases {
    plan_ms: u128,
    apply_ms: u128,
}

#[derive(Serialize)]
struct PrunePlanPayload {
    delete: Vec<u64>,
    retained: Vec<u64>,
    #[serde(flatten)]
    timing: Option<PruneTiming>,
}

fn prune_payload(report: &PruneReport, timings: &PruneTimings, timing: bool) -> PrunePayload {
    PrunePayload {
        pruned: report.pruned,
        rows_folded: report.rows_folded,
        rows_dropped: report.rows_dropped,
        timing: timing.then_some(PruneTiming {
            total_ms: timings.total.as_millis(),
            phases: PrunePhases {
                plan_ms: timings.plan.as_millis(),
                apply_ms: timings.apply.as_millis(),
            },
        }),
    }
}

fn prune_plan_payload(plan: &PrunePlan, timings: &PruneTimings, timing: bool) -> PrunePlanPayload {
    PrunePlanPayload {
        delete: plan.delete.iter().map(|id| id.raw()).collect(),
        retained: plan.retained.iter().map(|id| id.raw()).collect(),
        timing: timing.then_some(PruneTiming {
            total_ms: timings.total.as_millis(),
            phases: PrunePhases {
                plan_ms: timings.plan.as_millis(),
                apply_ms: timings.apply.as_millis(),
            },
        }),
    }
}

fn highlight_count(style: Styler, count: usize) -> String {
    let text = count.to_string();
    if count == 0 {
        text
    } else {
        style.yellow(&text)
    }
}

fn print_prune_timing_table(timings: &PruneTimings, style: Styler) {
    println!(
        "{}",
        style.dim(&format!(
            "timing total={}ms plan={}ms apply={}ms",
            timings.total.as_millis(),
            timings.plan.as_millis(),
            timings.apply.as_millis(),
        ))
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::time::Duration;

    fn timings() -> PruneTimings {
        PruneTimings {
            total: Duration::from_millis(30),
            plan: Duration::from_millis(20),
            apply: Duration::from_millis(10),
        }
    }

    #[test]
    fn renders_prune_payload() {
        let report = PruneReport {
            pruned: 2,
            rows_folded: 5,
            rows_dropped: 3,
        };
        let payload = prune_payload(&report, &timings(), false);
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            r#"{"pruned":2,"rows_folded":5,"rows_dropped":3}"#
        );
        let payload_timed = prune_payload(&report, &timings(), true);
        assert_eq!(
            serde_json::to_string(&payload_timed).unwrap(),
            r#"{"pruned":2,"rows_folded":5,"rows_dropped":3,"total_ms":30,"phases":{"plan_ms":20,"apply_ms":10}}"#
        );
    }

    #[test]
    fn renders_prune_plan_payload() {
        let plan = PrunePlan {
            delete: vec![sekai_app::SnapshotId(1)],
            retained: vec![sekai_app::SnapshotId(2), sekai_app::SnapshotId(3)],
        };
        let payload = prune_plan_payload(&plan, &timings(), false);
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            r#"{"delete":[1],"retained":[2,3]}"#
        );
        let payload_timed = prune_plan_payload(&plan, &timings(), true);
        assert_eq!(
            serde_json::to_string(&payload_timed).unwrap(),
            r#"{"delete":[1],"retained":[2,3],"total_ms":30,"phases":{"plan_ms":20,"apply_ms":10}}"#
        );
        assert_eq!(ids(&plan.delete), "1");
    }
}
