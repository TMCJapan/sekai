//! Status subcommand execution, DTOs, and output rendering.

use anyhow::Context as _;
use sekai_app::{StatusReport, StatusTimings};
use serde::Serialize;
use std::path::Path;

use super::ReportOut;
use super::progress::{finish_progress, progress_bar, report_progress};
use crate::cli::Selection;
use crate::envelope::envelope_ok;
use crate::style::Styler;

pub async fn run(
    store: &str,
    world: &Path,
    jobs: usize,
    progress: bool,
    selection: &Selection,
    out: ReportOut,
) -> anyhow::Result<()> {
    let scope = selection.owned_scope();
    let bar = progress_bar(progress);
    let owned = bar.clone();
    let options = sekai_app::StatusOptions { concurrency: jobs };
    let (report, timings) = sekai_app::status(world, store, options, scope, move |update| {
        report_progress(
            owned.as_ref(),
            update.files_done,
            update.files_total,
            format!("files {} chunks", update.chunks_done),
        );
    })
    .await
    .with_context(|| format!("status of {} failed", world.display()))?;
    finish_progress(bar.as_ref());
    if out.json {
        println!(
            "{}",
            envelope_ok("status", &status_payload(&report, &timings, out.timing))?
        );
        return Ok(());
    }
    let style = out.style;
    println!("{}", status_line(&report, style));
    if out.timing {
        print_status_timing_table(&timings, style);
    }
    Ok(())
}

fn status_line(report: &StatusReport, style: Styler) -> String {
    let basis = report.latest.map_or_else(
        || "no snapshot yet".to_owned(),
        |id| format!("snapshot {}", id.raw()),
    );
    if report.clean {
        return format!("clean: no changes since {basis}");
    }
    format!(
        "would record against {basis}: {} regions changed ({} new, {} deleted), {} chunks, {} tombstones, {} new blobs",
        style.bold(&report.changed_regions.to_string()),
        report.new_files,
        report.deleted_files,
        report.new_chunks,
        report.tombstones,
        report.new_blobs
    )
}

#[derive(Serialize)]
struct StatusPayload {
    clean: bool,
    latest: Option<u64>,
    changed_regions: usize,
    new_files: usize,
    deleted_files: usize,
    new_chunks: usize,
    tombstones: usize,
    new_blobs: usize,
    #[serde(flatten)]
    timing: Option<StatusTiming>,
}

#[derive(Serialize)]
struct StatusTiming {
    total_ms: u128,
    phases: StatusPhases,
}

#[allow(clippy::struct_field_names)]
#[derive(Serialize)]
struct StatusPhases {
    discover_ms: u128,
    fingerprint_ms: u128,
    universe_load_ms: u128,
    region_open_ms: u128,
    ingest_ms: u128,
    hash_ms: u128,
}

fn status_payload(report: &StatusReport, timings: &StatusTimings, timing: bool) -> StatusPayload {
    StatusPayload {
        clean: report.clean,
        latest: report.latest.map(sekai_app::SnapshotId::raw),
        changed_regions: report.changed_regions,
        new_files: report.new_files,
        deleted_files: report.deleted_files,
        new_chunks: report.new_chunks,
        tombstones: report.tombstones,
        new_blobs: report.new_blobs,
        timing: timing.then_some(StatusTiming {
            total_ms: timings.total.as_millis(),
            phases: StatusPhases {
                discover_ms: timings.discover.as_millis(),
                fingerprint_ms: timings.fingerprint.as_millis(),
                universe_load_ms: timings.universe_load.as_millis(),
                region_open_ms: timings.region_open.as_millis(),
                ingest_ms: timings.ingest.as_millis(),
                hash_ms: timings.hash.as_millis(),
            },
        }),
    }
}

fn print_status_timing_table(timings: &StatusTimings, style: Styler) {
    println!(
        "{}",
        style.dim(&format!(
            "timing total={}ms discover={}ms fingerprint={}ms universe_load={}ms region_open={}ms ingest={}ms hash={}ms",
            timings.total.as_millis(),
            timings.discover.as_millis(),
            timings.fingerprint.as_millis(),
            timings.universe_load.as_millis(),
            timings.region_open.as_millis(),
            timings.ingest.as_millis(),
            timings.hash.as_millis(),
        ))
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::time::Duration;

    fn report() -> StatusReport {
        StatusReport {
            clean: false,
            latest: Some(sekai_app::SnapshotId(2)),
            changed_regions: 1,
            new_files: 0,
            deleted_files: 0,
            new_chunks: 3,
            tombstones: 1,
            new_blobs: 2,
        }
    }

    fn timings() -> StatusTimings {
        StatusTimings {
            total: Duration::from_millis(50),
            discover: Duration::from_millis(1),
            fingerprint: Duration::from_millis(2),
            universe_load: Duration::from_millis(3),
            region_open: Duration::from_millis(4),
            ingest: Duration::from_millis(30),
            hash: Duration::from_millis(10),
        }
    }

    #[test]
    fn renders_status_payload() {
        let payload = status_payload(&report(), &timings(), false);
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            r#"{"clean":false,"latest":2,"changed_regions":1,"new_files":0,"deleted_files":0,"new_chunks":3,"tombstones":1,"new_blobs":2}"#
        );
        let payload_timed = status_payload(&report(), &timings(), true);
        assert_eq!(
            serde_json::to_string(&payload_timed).unwrap(),
            r#"{"clean":false,"latest":2,"changed_regions":1,"new_files":0,"deleted_files":0,"new_chunks":3,"tombstones":1,"new_blobs":2,"total_ms":50,"phases":{"discover_ms":1,"fingerprint_ms":2,"universe_load_ms":3,"region_open_ms":4,"ingest_ms":30,"hash_ms":10}}"#
        );
    }

    #[test]
    fn renders_status_lines() {
        let style = Styler::disabled();
        assert_eq!(
            status_line(&report(), style),
            "would record against snapshot 2: 1 regions changed (0 new, 0 deleted), 3 chunks, 1 tombstones, 2 new blobs"
        );
        assert_eq!(
            status_line(
                &StatusReport {
                    clean: true,
                    latest: None,
                    ..report()
                },
                style
            ),
            "clean: no changes since no snapshot yet"
        );
    }
}
