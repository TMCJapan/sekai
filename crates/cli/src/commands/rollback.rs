//! Rollback subcommand execution, DTOs, and output rendering.

use anyhow::Context as _;
use sekai_app::{RollbackReport, RollbackTimings};
use serde::Serialize;
use std::path::Path;

use super::ReportOut;
use super::progress::{finish_progress, progress_bar, report_progress};
use crate::cli::{OnMissingBlob, OnMissingFile, Selection};
use crate::envelope::envelope_ok;
use crate::style::Styler;

pub struct RollbackFlags {
    pub keep_post_snapshot_files: bool,
    pub keep_post_snapshot_chunks: bool,
    pub keep_tombstoned_chunks: bool,
    pub on_missing_blob: OnMissingBlob,
    pub on_missing_file: OnMissingFile,
}

const fn map_options(flags: &RollbackFlags) -> sekai_app::RollbackOptions {
    sekai_app::RollbackOptions {
        keep_post_snapshot_files: flags.keep_post_snapshot_files,
        keep_post_snapshot_chunks: flags.keep_post_snapshot_chunks,
        keep_tombstoned_chunks: flags.keep_tombstoned_chunks,
        on_missing_blob: match flags.on_missing_blob {
            OnMissingBlob::Abort => sekai_app::MissingBlobPolicy::Abort,
            OnMissingBlob::SkipChunk => sekai_app::MissingBlobPolicy::SkipChunk,
        },
        on_missing_file: match flags.on_missing_file {
            OnMissingFile::SiblingFirst => sekai_app::MissingFilePolicy::SiblingFirst,
            OnMissingFile::DerivedOnly => sekai_app::MissingFilePolicy::DerivedOnly,
            OnMissingFile::Error => sekai_app::MissingFilePolicy::Error,
        },
    }
}

pub async fn run(
    store: &str,
    world: &Path,
    snapshot: &str,
    progress: bool,
    selection: &Selection,
    flags: &RollbackFlags,
    out: ReportOut,
) -> anyhow::Result<()> {
    let id = sekai_app::resolve_snapshot_ref(store, snapshot)
        .await
        .with_context(|| format!("snapshot {snapshot:?} failed to resolve"))?;
    let scope = selection.owned_scope();
    let options = map_options(flags);
    if !out.json {
        eprintln!(
            "rollback {} to snapshot {} (scope: {}, policy: {})",
            world.display(),
            id.raw(),
            selection.describe(),
            policy_summary(flags),
        );
    }
    let bar = progress_bar(progress);
    let owned = bar.clone();
    let (report, timings) = sekai_app::rollback(world, store, id, options, scope, move |update| {
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
            "rollback of {} to snapshot {snapshot:?} failed",
            world.display()
        )
    })?;
    finish_progress(bar.as_ref());
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
        style.bold(&id.raw().to_string()),
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

/// One-line human summary of the restore policy for the pre-run echo.
fn policy_summary(flags: &RollbackFlags) -> String {
    let mut parts = Vec::new();
    if flags.keep_post_snapshot_files {
        parts.push("keep files");
    }
    if flags.keep_post_snapshot_chunks {
        parts.push("keep chunks");
    }
    if flags.keep_tombstoned_chunks {
        parts.push("keep tombstones");
    }
    if matches!(flags.on_missing_blob, OnMissingBlob::SkipChunk) {
        parts.push("skip missing blobs");
    }
    if !matches!(flags.on_missing_file, OnMissingFile::SiblingFirst) {
        parts.push(match flags.on_missing_file {
            OnMissingFile::SiblingFirst => "sibling-first",
            OnMissingFile::DerivedOnly => "derived-only",
            OnMissingFile::Error => "missing-file error",
        });
    }
    if parts.is_empty() {
        return "strict".to_owned();
    }
    parts.join(", ")
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

    #[test]
    fn maps_strategy_flags_to_options() {
        let strict = RollbackFlags {
            keep_post_snapshot_files: false,
            keep_post_snapshot_chunks: false,
            keep_tombstoned_chunks: false,
            on_missing_blob: OnMissingBlob::Abort,
            on_missing_file: OnMissingFile::SiblingFirst,
        };
        assert_eq!(map_options(&strict), sekai_app::RollbackOptions::default());

        let lenient = RollbackFlags {
            keep_post_snapshot_files: true,
            keep_post_snapshot_chunks: true,
            keep_tombstoned_chunks: true,
            on_missing_blob: OnMissingBlob::SkipChunk,
            on_missing_file: OnMissingFile::Error,
        };
        assert_eq!(
            map_options(&lenient),
            sekai_app::RollbackOptions {
                keep_post_snapshot_files: true,
                keep_post_snapshot_chunks: true,
                keep_tombstoned_chunks: true,
                on_missing_blob: sekai_app::MissingBlobPolicy::SkipChunk,
                on_missing_file: sekai_app::MissingFilePolicy::Error,
            }
        );
    }

    #[test]
    fn summarizes_restore_policy() {
        let strict = RollbackFlags {
            keep_post_snapshot_files: false,
            keep_post_snapshot_chunks: false,
            keep_tombstoned_chunks: false,
            on_missing_blob: OnMissingBlob::Abort,
            on_missing_file: OnMissingFile::SiblingFirst,
        };
        assert_eq!(policy_summary(&strict), "strict");

        let lenient = RollbackFlags {
            keep_post_snapshot_files: true,
            keep_post_snapshot_chunks: false,
            keep_tombstoned_chunks: true,
            on_missing_blob: OnMissingBlob::SkipChunk,
            on_missing_file: OnMissingFile::Error,
        };
        assert_eq!(
            policy_summary(&lenient),
            "keep files, keep tombstones, skip missing blobs, missing-file error"
        );
    }
}
