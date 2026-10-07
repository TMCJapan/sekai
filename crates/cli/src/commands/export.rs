//! Export subcommand execution, DTOs, and output rendering.

use anyhow::Context as _;
use sekai_app::{ExportReport, ExportTimings, HostWorldTree};
use serde::Serialize;
use std::path::Path;

use super::ReportOut;
use super::progress::{finish_progress, progress_bar, report_progress};
use crate::cli::{ExportFlavor, OnMissingBlob, Selection, SnapshotRef};
use crate::envelope::envelope_ok;
use crate::style::Styler;

pub struct ExportFlags {
    pub flavor: ExportFlavor,
    pub base: String,
    pub on_missing_blob: OnMissingBlob,
}

/// World tree matching the requested output layout. Export targets do not
/// exist yet, so the layout cannot be detected and must be chosen.
fn output_tree(flags: &ExportFlags, out_dir: impl AsRef<Path>) -> HostWorldTree {
    match flags.flavor {
        ExportFlavor::Legacy => HostWorldTree::new_legacy(out_dir),
        ExportFlavor::New => HostWorldTree::new_dimensions(out_dir),
        ExportFlavor::Bukkit => HostWorldTree::new_bukkit(out_dir, flags.base.clone()),
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
    snapshot: &SnapshotRef,
    out_dir: impl AsRef<Path>,
    progress: bool,
    selection: &Selection,
    flags: &ExportFlags,
    out: ReportOut,
) -> anyhow::Result<()> {
    let world = output_tree(flags, out_dir.as_ref());
    let instance = sekai_app::SekaiInstance::open(store)
        .await
        .with_context(|| {
            format!(
                "export of snapshot {snapshot} to {} failed",
                out_dir.as_ref().display()
            )
        })?;
    let id = instance
        .resolve_snapshot_ref(snapshot.as_str())
        .await
        .with_context(|| format!("snapshot {snapshot} failed to resolve"))?;
    let scope = selection.owned_scope();
    let options = map_options(flags.on_missing_blob);
    let bar = progress_bar(progress);
    let owned = bar.clone();
    let (report, timings) = instance
        .export(&world, id, options, scope, move |update| {
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
                "export of snapshot {snapshot} to {} failed",
                out_dir.as_ref().display()
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
    fn builds_output_tree_per_flavor() {
        use sekai_app::{Dimension, RegionKind, WorldTree as _};
        let flags = |flavor, base: &str| ExportFlags {
            flavor,
            base: base.to_owned(),
            on_missing_blob: OnMissingBlob::Abort,
        };

        let legacy = output_tree(&flags(ExportFlavor::Legacy, "ignored"), "/out");
        assert_eq!(
            legacy
                .derive_path(Dimension::NETHER, RegionKind::REGION, 0, 0)
                .unwrap(),
            Path::new("/out/DIM-1/region/r.0.0.mca")
        );

        let modern = output_tree(&flags(ExportFlavor::New, "ignored"), "/out");
        assert_eq!(
            modern
                .derive_path(Dimension::END, RegionKind::POI, 0, 0)
                .unwrap(),
            Path::new("/out/dimensions/minecraft/the_end/poi/r.0.0.mca")
        );

        let bukkit = output_tree(&flags(ExportFlavor::Bukkit, "myworld"), "/out");
        assert_eq!(
            bukkit
                .derive_path(Dimension::OVERWORLD, RegionKind::REGION, 1, 2)
                .unwrap(),
            Path::new("/out/myworld/region/r.1.2.mca")
        );
    }

    #[test]
    fn maps_missing_blob_policy() {
        for (mode, expected) in [
            (OnMissingBlob::Abort, sekai_app::MissingBlobPolicy::Abort),
            (
                OnMissingBlob::SkipChunk,
                sekai_app::MissingBlobPolicy::SkipChunk,
            ),
        ] {
            assert_eq!(
                map_options(mode),
                sekai_app::ExportOptions {
                    on_missing_blob: expected,
                }
            );
        }
    }
}
