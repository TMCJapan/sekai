//! Clone subcommand execution, DTOs, and output rendering.

use anyhow::Context as _;
use sekai_app::{CloneReport, CloneTimings, HostWorldTree};
use serde::Serialize;
use std::path::Path;

use super::ReportOut;
use super::progress::{finish_progress, progress_bar, report_progress};
use crate::cli::{CloneFlavor, CommitRef, OnMissingBlob, Selection};
use crate::envelope::envelope_ok;
use crate::style::Styler;

pub struct CloneFlags {
    pub flavor: CloneFlavor,
    pub base: String,
    pub on_missing_blob: OnMissingBlob,
}

/// World tree matching the requested output layout. Clone targets do not
/// exist yet, so the layout cannot be detected and must be chosen.
fn output_tree(flags: &CloneFlags, out_dir: impl AsRef<Path>) -> HostWorldTree {
    match flags.flavor {
        CloneFlavor::Legacy => HostWorldTree::new_legacy(out_dir),
        CloneFlavor::New => HostWorldTree::new_dimensions(out_dir),
        CloneFlavor::Bukkit => HostWorldTree::new_bukkit(out_dir, flags.base.clone()),
    }
}

const fn map_options(on_missing_blob: OnMissingBlob) -> sekai_app::CloneOptions {
    sekai_app::CloneOptions {
        on_missing_blob: match on_missing_blob {
            OnMissingBlob::Abort => sekai_app::MissingBlobPolicy::Abort,
            OnMissingBlob::SkipChunk => sekai_app::MissingBlobPolicy::SkipChunk,
        },
    }
}

pub async fn run(
    store: &str,
    commit: &CommitRef,
    out_dir: impl AsRef<Path>,
    progress: bool,
    selection: &Selection,
    flags: &CloneFlags,
    out: ReportOut,
) -> anyhow::Result<()> {
    let world = output_tree(flags, out_dir.as_ref());
    let instance = sekai_app::SekaiInstance::open(store)
        .await
        .with_context(|| {
            format!(
                "clone of commit {commit} to {} failed",
                out_dir.as_ref().display()
            )
        })?;
    let id = instance
        .resolve_commit_ref(commit.as_str())
        .await
        .with_context(|| format!("commit {commit} failed to resolve"))?;
    let scope = selection.owned_scope();
    let options = map_options(flags.on_missing_blob);
    let bar = progress_bar(progress);
    let owned = bar.clone();
    let (report, timings) = instance
        .clone_to(&world, id, options, scope, move |update| {
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
                "clone of commit {commit} to {} failed",
                out_dir.as_ref().display()
            )
        })?;
    finish_progress(bar.as_ref());
    if out.json {
        println!(
            "{}",
            envelope_ok("clone", &clone_payload(&report, &timings, out.timing))?
        );
        return Ok(());
    }
    let style = out.style;
    println!(
        "commit {} cloned: {} files written, {} chunks restored",
        style.bold(&id.to_string()),
        report.files_written,
        report.chunks_restored
    );
    if out.timing {
        print_clone_timing_table(&timings, style);
    }
    Ok(())
}

#[derive(Serialize)]
struct ClonePayload {
    files_written: usize,
    chunks_restored: usize,
    #[serde(flatten)]
    timing: Option<CloneTiming>,
}

#[derive(Serialize)]
struct CloneTiming {
    total_ms: u128,
    phases: ClonePhases,
}

#[allow(clippy::struct_field_names)]
#[derive(Serialize)]
struct ClonePhases {
    plan_ms: u128,
    clone_files_ms: u128,
}

fn clone_payload(report: &CloneReport, timings: &CloneTimings, timing: bool) -> ClonePayload {
    ClonePayload {
        files_written: report.files_written,
        chunks_restored: report.chunks_restored,
        timing: timing.then_some(CloneTiming {
            total_ms: timings.total.as_millis(),
            phases: ClonePhases {
                plan_ms: timings.plan.as_millis(),
                clone_files_ms: timings.clone_files.as_millis(),
            },
        }),
    }
}

fn print_clone_timing_table(timings: &CloneTimings, style: Styler) {
    println!(
        "{}",
        style.dim(&format!(
            "timing total={}ms plan={}ms clone_files={}ms",
            timings.total.as_millis(),
            timings.plan.as_millis(),
            timings.clone_files.as_millis(),
        ))
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::time::Duration;

    #[test]
    fn renders_clone_payload() {
        let report = CloneReport {
            files_written: 2,
            chunks_restored: 40,
        };
        let timings = CloneTimings {
            total: Duration::from_millis(90),
            plan: Duration::from_millis(9),
            clone_files: Duration::from_millis(70),
        };
        let payload = clone_payload(&report, &timings, false);
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            r#"{"files_written":2,"chunks_restored":40}"#
        );
        let payload_timed = clone_payload(&report, &timings, true);
        assert_eq!(
            serde_json::to_string(&payload_timed).unwrap(),
            r#"{"files_written":2,"chunks_restored":40,"total_ms":90,"phases":{"plan_ms":9,"clone_files_ms":70}}"#
        );
    }

    #[test]
    fn builds_output_tree_per_flavor() {
        use sekai_app::{Dimension, RegionKey, RegionKind, WorldTree as _};
        let flags = |flavor, base: &str| CloneFlags {
            flavor,
            base: base.to_owned(),
            on_missing_blob: OnMissingBlob::Abort,
        };

        let legacy = output_tree(&flags(CloneFlavor::Legacy, "ignored"), "/out");
        assert_eq!(
            legacy
                .derive_region(RegionKey::new(Dimension::NETHER, RegionKind::Region, 0, 0))
                .unwrap()
                .as_path(),
            Path::new("/out/DIM-1/region/r.0.0.mca")
        );

        let modern = output_tree(&flags(CloneFlavor::New, "ignored"), "/out");
        assert_eq!(
            modern
                .derive_region(RegionKey::new(Dimension::END, RegionKind::Poi, 0, 0))
                .unwrap()
                .as_path(),
            Path::new("/out/dimensions/minecraft/the_end/poi/r.0.0.mca")
        );

        let bukkit = output_tree(&flags(CloneFlavor::Bukkit, "myworld"), "/out");
        assert_eq!(
            bukkit
                .derive_region(RegionKey::new(
                    Dimension::OVERWORLD,
                    RegionKind::Region,
                    1,
                    2
                ))
                .unwrap()
                .as_path(),
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
                sekai_app::CloneOptions {
                    on_missing_blob: expected,
                }
            );
        }
    }
}
