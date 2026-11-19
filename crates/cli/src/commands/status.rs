//! Status subcommand execution, DTOs, and output rendering.

use sekai_app::{HeadState, Location, StatusReport, StatusTimings};
use serde::Serialize;

use super::progress::Bars;
use super::{HeadJson, ReportOut, resolve_selection};
use crate::cli::Selection;
use crate::envelope::Printer;
use crate::style::Styler;

pub async fn run(
    location: &Location,
    jobs: usize,
    selection: &Selection,
    out: ReportOut,
    printer: &Printer,
) -> anyhow::Result<()> {
    let options = sekai_app::StatusOptions { concurrency: jobs };
    let instance = sekai_app::SekaiInstance::open_location(location).await?;
    let world = location.world_tree()?;
    let scope = resolve_selection(&instance, selection).await?;
    let bars = Bars::new(out.progress);
    let (report, timings) = instance.world(&world).status(options, scope, &bars).await?;
    let head = instance.head().await?;
    if out.json {
        let payload = status_payload(&report, &timings, head.as_ref(), out.timing);
        printer.print_ok("status", &payload)?;
        return Ok(());
    }
    let style = out.style;
    if let Some(head) = &head {
        println!("{}", head_lines(head));
    }
    println!("{}", status_line(&report, style));
    if out.timing {
        print_status_timing_table(&timings, style);
    }
    Ok(())
}

/// Git's first lines of `status`: where HEAD is, and `No commits yet`
/// on an unborn branch. A detached HEAD prints its commit (ADR-0036
/// *status*).
fn head_lines(head: &HeadState) -> String {
    match head {
        HeadState::Attached { branch, .. } => format!("On branch {branch}"),
        HeadState::Unborn { branch } => format!("On branch {branch}\n\nNo commits yet\n"),
        HeadState::Detached { commit } => format!("HEAD detached at {commit}"),
    }
}

fn status_line(report: &StatusReport, style: Styler) -> String {
    let basis = report
        .latest
        .map_or_else(|| "no commit yet".to_owned(), |id| format!("commit {id}"));
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
    latest: Option<String>,
    changed_regions: usize,
    new_files: usize,
    deleted_files: usize,
    new_chunks: usize,
    tombstones: usize,
    new_blobs: usize,
    /// HEAD the preview is measured against (ADR-0036 *Output*).
    head: Option<HeadJson>,
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
    universe_load_ms: u128,
    region_open_ms: u128,
    ingest_ms: u128,
    hash_ms: u128,
}

fn status_payload(
    report: &StatusReport,
    timings: &StatusTimings,
    head: Option<&HeadState>,
    timing: bool,
) -> StatusPayload {
    StatusPayload {
        clean: report.clean,
        latest: report.latest.map(|id| id.to_string()),
        changed_regions: report.changed_regions,
        new_files: report.new_files,
        deleted_files: report.deleted_files,
        new_chunks: report.new_chunks,
        tombstones: report.tombstones,
        new_blobs: report.new_blobs,
        head: head.map(HeadJson::from),
        timing: timing.then_some(StatusTiming {
            total_ms: timings.total.as_millis(),
            phases: StatusPhases {
                discover_ms: timings.discover.as_millis(),
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
            "timing total={}ms discover={}ms universe_load={}ms region_open={}ms ingest={}ms hash={}ms",
            timings.total.as_millis(),
            timings.discover.as_millis(),
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
            latest: Some(sekai_app::CommitId([2; 32])),
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
            universe_load: Duration::from_millis(3),
            region_open: Duration::from_millis(4),
            ingest: Duration::from_millis(30),
            hash: Duration::from_millis(10),
        }
    }

    #[test]
    fn renders_status_payload() {
        let latest = sekai_app::CommitId([2; 32]);
        let head = HeadState::Unborn {
            branch: sekai_app::RefName::parse("main").unwrap(),
        };
        let payload = status_payload(&report(), &timings(), Some(&head), false);
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            format!(
                r#"{{"clean":false,"latest":"{latest}","changed_regions":1,"new_files":0,"deleted_files":0,"new_chunks":3,"tombstones":1,"new_blobs":2,"head":{{"ref":"refs/heads/main","commit":null}}}}"#
            )
        );
        let payload_timed = status_payload(&report(), &timings(), None, true);
        assert_eq!(
            serde_json::to_string(&payload_timed).unwrap(),
            format!(
                r#"{{"clean":false,"latest":"{latest}","changed_regions":1,"new_files":0,"deleted_files":0,"new_chunks":3,"tombstones":1,"new_blobs":2,"head":null,"total_ms":50,"phases":{{"discover_ms":1,"universe_load_ms":3,"region_open_ms":4,"ingest_ms":30,"hash_ms":10}}}}"#
            )
        );
    }

    #[test]
    fn renders_status_lines() {
        let style = Styler::disabled();
        assert_eq!(
            status_line(&report(), style),
            format!(
                "would record against commit {}: 1 regions changed (0 new, 0 deleted), 3 chunks, 1 tombstones, 2 new blobs",
                sekai_app::CommitId([2; 32])
            )
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
            "clean: no changes since no commit yet"
        );
    }
}
