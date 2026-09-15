//! Rollback subcommand execution, DTOs, and output rendering.

use anyhow::Context as _;
use sekai_app::{RollbackReport, RollbackTimings, SnapshotId};
use serde::Serialize;
use std::path::Path;

use super::ReportOut;
use crate::cli::Selection;
use crate::envelope::envelope_ok;
use crate::style::Styler;

pub async fn run(
    store: &str,
    world: &Path,
    snapshot: u64,
    selection: &Selection,
    out: ReportOut,
) -> anyhow::Result<()> {
    let id = SnapshotId(snapshot);
    let scope = selection.owned_scope();
    let (report, timings) = sekai_app::rollback(world, store, id, (&scope).into())
        .await
        .with_context(|| {
            format!(
                "rollback of {} to snapshot {snapshot} failed",
                world.display()
            )
        })?;
    if out.json {
        println!(
            "{}",
            envelope_ok("rollback", &rollback_payload(&report, &timings, out.timing))?
        );
        return Ok(());
    }
    let style = out.style;
    println!(
        "snapshot {} restored: {} files rewritten, {} files deleted, {} chunks restored",
        style.bold(&snapshot.to_string()),
        report.files_written,
        report.files_deleted,
        report.chunks_restored
    );
    if out.timing {
        print_rollback_timing_table(&timings, style);
    }
    Ok(())
}

#[derive(Serialize)]
struct RollbackPayload {
    files_written: usize,
    files_deleted: usize,
    chunks_restored: usize,
    #[serde(flatten)]
    timing: Option<RollbackTiming>,
}

#[derive(Serialize)]
struct RollbackTiming {
    total_ms: u128,
    phases: RollbackPhases,
}

#[allow(clippy::struct_field_names)]
#[derive(Serialize)]
struct RollbackPhases {
    plan_ms: u128,
    discover_ms: u128,
    rollback_files_ms: u128,
}

fn rollback_payload(
    report: &RollbackReport,
    timings: &RollbackTimings,
    timing: bool,
) -> RollbackPayload {
    RollbackPayload {
        files_written: report.files_written,
        files_deleted: report.files_deleted,
        chunks_restored: report.chunks_restored,
        timing: timing.then_some(RollbackTiming {
            total_ms: timings.total.as_millis(),
            phases: RollbackPhases {
                plan_ms: timings.plan.as_millis(),
                discover_ms: timings.discover.as_millis(),
                rollback_files_ms: timings.rollback_files.as_millis(),
            },
        }),
    }
}

fn print_rollback_timing_table(timings: &RollbackTimings, style: Styler) {
    println!(
        "{}",
        style.dim(&format!(
            "timing total={}ms plan={}ms discover={}ms rollback_files={}ms",
            timings.total.as_millis(),
            timings.plan.as_millis(),
            timings.discover.as_millis(),
            timings.rollback_files.as_millis(),
        ))
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::time::Duration;

    #[test]
    fn renders_rollback_payload() {
        let report = RollbackReport {
            files_written: 2,
            files_deleted: 1,
            chunks_restored: 40,
        };
        let timings = RollbackTimings {
            total: Duration::from_millis(90),
            plan: Duration::from_millis(9),
            discover: Duration::from_millis(8),
            rollback_files: Duration::from_millis(70),
        };
        let payload = rollback_payload(&report, &timings, false);
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            r#"{"files_written":2,"files_deleted":1,"chunks_restored":40}"#
        );
        let payload_timed = rollback_payload(&report, &timings, true);
        assert_eq!(
            serde_json::to_string(&payload_timed).unwrap(),
            r#"{"files_written":2,"files_deleted":1,"chunks_restored":40,"total_ms":90,"phases":{"plan_ms":9,"discover_ms":8,"rollback_files_ms":70}}"#
        );
    }
}
