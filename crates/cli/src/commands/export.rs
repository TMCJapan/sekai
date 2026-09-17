//! Export subcommand execution, DTOs, and output rendering.

use anyhow::Context as _;
use sekai_app::{ExportReport, ExportTimings};
use serde::Serialize;
use std::path::Path;

use super::ReportOut;
use super::progress::{finish_progress, progress_bar, report_progress};
use crate::cli::{ExportFlavor, OnMissingBlob, Selection};
use crate::envelope::envelope_ok;
use crate::style::Styler;

pub struct ExportFlags {
    pub flavor: ExportFlavor,
    pub base: String,
    pub on_missing_blob: OnMissingBlob,
}

fn map_flavor(flags: &ExportFlags) -> sekai_app::LayoutFlavor {
    match flags.flavor {
        ExportFlavor::Legacy => sekai_app::LayoutFlavor::Legacy,
        ExportFlavor::New => sekai_app::LayoutFlavor::New,
        ExportFlavor::Bukkit => sekai_app::LayoutFlavor::Bukkit {
            base: flags.base.clone(),
        },
    }
}

const fn map_options(on_missing_blob: OnMissingBlob) -> sekai_app::ExportOptions {
    sekai_app::ExportOptions {
        on_missing_blob: match on_missing_blob {
            OnMissingBlob::Abort => sekai_app::MissingBlobPolicy::Abort,
            OnMissingBlob::SkipChunk => sekai_app::MissingBlobPolicy::SkipChunk,
        },
    }
}

pub async fn run(
    store: &str,
    snapshot: &str,
    out_dir: &Path,
    progress: bool,
    selection: &Selection,
    flags: &ExportFlags,
    out: ReportOut,
) -> anyhow::Result<()> {
    let id = sekai_app::resolve_snapshot_ref(store, snapshot)
        .await
        .with_context(|| format!("snapshot {snapshot:?} failed to resolve"))?;
    let scope = selection.owned_scope();
    let flavor = map_flavor(flags);
    let options = map_options(flags.on_missing_blob);
    let bar = progress_bar(progress);
    let owned = bar.clone();
    let (report, timings) =
        sekai_app::export(out_dir, store, id, flavor, options, scope, move |update| {
            report_progress(
                owned.as_ref(),
                update.files_done,
                update.files_total,
                format!("files {} chunks", update.chunks_done),
            );
        })
        .await
        .with_context(|| {
            format!(
                "export of snapshot {snapshot:?} to {} failed",
                out_dir.display()
            )
        })?;
    finish_progress(bar.as_ref());
    if out.json {
        println!(
            "{}",
            envelope_ok("export", &export_payload(&report, &timings, out.timing))?
        );
        return Ok(());
    }
    let style = out.style;
    println!(
        "snapshot {} exported: {} files written, {} chunks restored",
        style.bold(&id.raw().to_string()),
        report.files_written,
        report.chunks_restored
    );
    if out.timing {
        print_export_timing_table(&timings, style);
    }
    Ok(())
}

#[derive(Serialize)]
struct ExportPayload {
    files_written: usize,
    chunks_restored: usize,
    #[serde(flatten)]
    timing: Option<ExportTiming>,
}

#[derive(Serialize)]
struct ExportTiming {
    total_ms: u128,
    phases: ExportPhases,
}

#[allow(clippy::struct_field_names)]
#[derive(Serialize)]
struct ExportPhases {
    plan_ms: u128,
    export_files_ms: u128,
}

fn export_payload(report: &ExportReport, timings: &ExportTimings, timing: bool) -> ExportPayload {
    ExportPayload {
        files_written: report.files_written,
        chunks_restored: report.chunks_restored,
        timing: timing.then_some(ExportTiming {
            total_ms: timings.total.as_millis(),
            phases: ExportPhases {
                plan_ms: timings.plan.as_millis(),
                export_files_ms: timings.export_files.as_millis(),
            },
        }),
    }
}

fn print_export_timing_table(timings: &ExportTimings, style: Styler) {
    println!(
        "{}",
        style.dim(&format!(
            "timing total={}ms plan={}ms export_files={}ms",
            timings.total.as_millis(),
            timings.plan.as_millis(),
            timings.export_files.as_millis(),
        ))
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::time::Duration;

    #[test]
    fn renders_export_payload() {
        let report = ExportReport {
            files_written: 2,
            chunks_restored: 40,
        };
        let timings = ExportTimings {
            total: Duration::from_millis(90),
            plan: Duration::from_millis(9),
            export_files: Duration::from_millis(70),
        };
        let payload = export_payload(&report, &timings, false);
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            r#"{"files_written":2,"chunks_restored":40}"#
        );
        let payload_timed = export_payload(&report, &timings, true);
        assert_eq!(
            serde_json::to_string(&payload_timed).unwrap(),
            r#"{"files_written":2,"chunks_restored":40,"total_ms":90,"phases":{"plan_ms":9,"export_files_ms":70}}"#
        );
    }

    #[test]
    fn maps_export_flavor() {
        let base = ExportFlags {
            flavor: ExportFlavor::Bukkit,
            base: "myworld".to_owned(),
            on_missing_blob: OnMissingBlob::Abort,
        };
        assert_eq!(
            map_flavor(&base),
            sekai_app::LayoutFlavor::Bukkit {
                base: "myworld".to_owned()
            }
        );
        for (flavor, expected) in [
            (ExportFlavor::Legacy, sekai_app::LayoutFlavor::Legacy),
            (ExportFlavor::New, sekai_app::LayoutFlavor::New),
        ] {
            let flags = ExportFlags {
                flavor,
                base: "ignored".to_owned(),
                on_missing_blob: OnMissingBlob::SkipChunk,
            };
            assert_eq!(map_flavor(&flags), expected);
            assert_eq!(
                map_options(flags.on_missing_blob),
                sekai_app::ExportOptions {
                    on_missing_blob: sekai_app::MissingBlobPolicy::SkipChunk,
                }
            );
        }
    }
}
