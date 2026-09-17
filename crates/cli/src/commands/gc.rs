//! Garbage collection subcommand execution, DTOs, and output rendering.

use anyhow::Context as _;
use sekai_app::{GcPlan, GcReport, GcTimings};
use serde::Serialize;

use super::ReportOut;
use super::progress::{finish_progress, progress_bar, report_progress};
use crate::envelope::envelope_ok;
use crate::style::Styler;

pub async fn run(store: &str, dry_run: bool, progress: bool, out: ReportOut) -> anyhow::Result<()> {
    let style = out.style;
    if dry_run {
        let (plan, timings) = sekai_app::gc_plan(store)
            .await
            .with_context(|| format!("gc plan for {store} failed"))?;
        if out.json {
            println!(
                "{}",
                envelope_ok("gc", &gc_plan_payload(&plan, &timings, out.timing))?
            );
            return Ok(());
        }
        println!(
            "gc plan created: {} orphan blobs (examined {})",
            highlight_count(style, plan.orphans.len()),
            plan.examined
        );
        if out.timing {
            print_gc_timing_table(&timings, style);
        }
        return Ok(());
    }

    let bar = progress_bar(progress);
    let (report, timings) = sekai_app::gc(store, |update| {
        report_progress(bar.as_ref(), update.blobs_done, update.blobs_total, "blobs");
    })
    .await
    .with_context(|| format!("gc for {store} failed"))?;
    finish_progress(bar.as_ref());
    if out.json {
        println!(
            "{}",
            envelope_ok("gc", &gc_payload(&report, &timings, out.timing))?
        );
        return Ok(());
    }
    println!(
        "gc completed: {} removed, {} orphans, {} candidates",
        highlight_count(style, report.removed),
        report.orphans,
        report.candidates
    );
    if out.timing {
        print_gc_timing_table(&timings, style);
    }
    Ok(())
}

#[derive(Serialize)]
struct GcPayload {
    candidates: usize,
    orphans: usize,
    removed: usize,
    #[serde(flatten)]
    timing: Option<GcTiming>,
}

#[derive(Serialize)]
struct GcTiming {
    total_ms: u128,
    phases: GcPhases,
}

#[allow(clippy::struct_field_names)]
#[derive(Serialize)]
struct GcPhases {
    plan_ms: u128,
    apply_ms: u128,
}

#[derive(Serialize)]
struct GcPlanPayload {
    orphans: usize,
    examined: usize,
    #[serde(flatten)]
    timing: Option<GcTiming>,
}

fn gc_payload(report: &GcReport, timings: &GcTimings, timing: bool) -> GcPayload {
    GcPayload {
        candidates: report.candidates,
        orphans: report.orphans,
        removed: report.removed,
        timing: timing.then_some(GcTiming {
            total_ms: timings.total.as_millis(),
            phases: GcPhases {
                plan_ms: timings.plan.as_millis(),
                apply_ms: timings.apply.as_millis(),
            },
        }),
    }
}

fn gc_plan_payload(plan: &GcPlan, timings: &GcTimings, timing: bool) -> GcPlanPayload {
    GcPlanPayload {
        orphans: plan.orphans.len(),
        examined: plan.examined,
        timing: timing.then_some(GcTiming {
            total_ms: timings.total.as_millis(),
            phases: GcPhases {
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

fn print_gc_timing_table(timings: &GcTimings, style: Styler) {
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

    #[test]
    fn highlight_count_emphasizes_nonzero() {
        let style = Styler::enabled();
        assert_eq!(highlight_count(style, 0), "0");
        assert_eq!(highlight_count(style, 7), "\x1b[33m7\x1b[0m");
        assert_eq!(highlight_count(Styler::disabled(), 7), "7");
    }

    #[test]
    fn renders_gc_payload() {
        let report = GcReport {
            candidates: 5,
            orphans: 4,
            removed: 4,
        };
        let timings = GcTimings {
            total: Duration::from_millis(30),
            plan: Duration::from_millis(20),
            apply: Duration::from_millis(10),
        };
        let payload = gc_payload(&report, &timings, false);
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            r#"{"candidates":5,"orphans":4,"removed":4}"#
        );
        let payload_timed = gc_payload(&report, &timings, true);
        assert_eq!(
            serde_json::to_string(&payload_timed).unwrap(),
            r#"{"candidates":5,"orphans":4,"removed":4,"total_ms":30,"phases":{"plan_ms":20,"apply_ms":10}}"#
        );
    }

    #[test]
    fn renders_gc_plan_payload() {
        let plan = GcPlan::new(vec![sekai_app::BlobHash([1; 32])], 10);
        let timings = GcTimings {
            total: Duration::from_millis(20),
            plan: Duration::from_millis(20),
            apply: Duration::ZERO,
        };
        let payload = gc_plan_payload(&plan, &timings, false);
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            r#"{"orphans":1,"examined":10}"#
        );
        let payload_timed = gc_plan_payload(&plan, &timings, true);
        assert_eq!(
            serde_json::to_string(&payload_timed).unwrap(),
            r#"{"orphans":1,"examined":10,"total_ms":20,"phases":{"plan_ms":20,"apply_ms":0}}"#
        );
    }
}
