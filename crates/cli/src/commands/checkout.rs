//! Checkout subcommand execution, DTOs, and output rendering.

use anyhow::Context as _;
use sekai_app::{CheckoutReport, CheckoutTimings, HostWorldTree};
use serde::Serialize;
use std::path::Path;

use super::ReportOut;
use super::progress::{finish_progress, progress_bar, report_progress};
use crate::cli::{CommitRef, OnMissingBlob, OnMissingFile, Selection};
use crate::envelope::envelope_ok;
use crate::style::Styler;

pub struct CheckoutFlags {
    pub keep_post_commit_files: bool,
    pub keep_post_commit_chunks: bool,
    pub keep_tombstoned_chunks: bool,
    pub on_missing_blob: OnMissingBlob,
    pub on_missing_file: OnMissingFile,
}

const fn map_options(flags: &CheckoutFlags) -> sekai_app::CheckoutOptions {
    sekai_app::CheckoutOptions {
        keep_post_commit_files: flags.keep_post_commit_files,
        keep_post_commit_chunks: flags.keep_post_commit_chunks,
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
    world_path: impl AsRef<Path>,
    commit: &CommitRef,
    progress: bool,
    selection: &Selection,
    flags: &CheckoutFlags,
    out: ReportOut,
) -> anyhow::Result<()> {
    let mut world = HostWorldTree::new(&world_path)
        .with_context(|| format!("failed to inspect world {}", world_path.as_ref().display()))?;
    let mut instance = sekai_app::SekaiInstance::open(store)
        .await
        .with_context(|| format!("checkout of {} failed", world_path.as_ref().display()))?;
    let id = instance
        .resolve_commit_ref(commit.as_str())
        .await
        .with_context(|| format!("commit {commit} failed to resolve"))?;
    let scope = selection.owned_scope();
    let options = map_options(flags);
    if !out.json {
        eprintln!(
            "checkout {} to commit {} (scope: {}, policy: {})",
            world_path.as_ref().display(),
            id,
            selection.describe(),
            policy_summary(flags),
        );
    }
    let bar = progress_bar(progress);
    let owned = bar.clone();
    let (report, timings) = instance
        .world_mut(&mut world)
        .checkout(id, options, scope, move |update| {
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
                "checkout of {} to commit {commit} failed",
                world_path.as_ref().display()
            )
        })?;
    finish_progress(bar.as_ref());
    if out.json {
        println!(
            "{}",
            envelope_ok("checkout", &checkout_payload(&report, &timings, out.timing))?
        );
        return Ok(());
    }
    let style = out.style;
    println!(
        "commit {} restored: {} files rewritten, {} files deleted, {} chunks restored",
        style.bold(&id.to_string()),
        report.files_written,
        report.files_deleted,
        report.chunks_restored
    );
    if out.timing {
        print_checkout_timing_table(&timings, style);
    }
    Ok(())
}

#[derive(Serialize)]
struct CheckoutPayload {
    files_written: usize,
    files_deleted: usize,
    chunks_restored: usize,
    #[serde(flatten)]
    timing: Option<CheckoutTiming>,
}

#[derive(Serialize)]
struct CheckoutTiming {
    total_ms: u128,
    phases: CheckoutPhases,
}

#[allow(clippy::struct_field_names)]
#[derive(Serialize)]
struct CheckoutPhases {
    plan_ms: u128,
    discover_ms: u128,
    checkout_files_ms: u128,
}

fn checkout_payload(
    report: &CheckoutReport,
    timings: &CheckoutTimings,
    timing: bool,
) -> CheckoutPayload {
    CheckoutPayload {
        files_written: report.files_written,
        files_deleted: report.files_deleted,
        chunks_restored: report.chunks_restored,
        timing: timing.then_some(CheckoutTiming {
            total_ms: timings.total.as_millis(),
            phases: CheckoutPhases {
                plan_ms: timings.plan.as_millis(),
                discover_ms: timings.discover.as_millis(),
                checkout_files_ms: timings.checkout_files.as_millis(),
            },
        }),
    }
}

fn print_checkout_timing_table(timings: &CheckoutTimings, style: Styler) {
    println!(
        "{}",
        style.dim(&format!(
            "timing total={}ms plan={}ms discover={}ms checkout_files={}ms",
            timings.total.as_millis(),
            timings.plan.as_millis(),
            timings.discover.as_millis(),
            timings.checkout_files.as_millis(),
        ))
    );
}

/// One-line human summary of the restore policy for the pre-run echo.
fn policy_summary(flags: &CheckoutFlags) -> String {
    let mut parts = Vec::new();
    if flags.keep_post_commit_files {
        parts.push("keep files");
    }
    if flags.keep_post_commit_chunks {
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
    fn renders_checkout_payload() {
        let report = CheckoutReport {
            files_written: 2,
            files_deleted: 1,
            chunks_restored: 40,
        };
        let timings = CheckoutTimings {
            total: Duration::from_millis(90),
            plan: Duration::from_millis(9),
            discover: Duration::from_millis(8),
            checkout_files: Duration::from_millis(70),
        };
        let payload = checkout_payload(&report, &timings, false);
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            r#"{"files_written":2,"files_deleted":1,"chunks_restored":40}"#
        );
        let payload_timed = checkout_payload(&report, &timings, true);
        assert_eq!(
            serde_json::to_string(&payload_timed).unwrap(),
            r#"{"files_written":2,"files_deleted":1,"chunks_restored":40,"total_ms":90,"phases":{"plan_ms":9,"discover_ms":8,"checkout_files_ms":70}}"#
        );
    }

    #[test]
    fn maps_strategy_flags_to_options() {
        let strict = CheckoutFlags {
            keep_post_commit_files: false,
            keep_post_commit_chunks: false,
            keep_tombstoned_chunks: false,
            on_missing_blob: OnMissingBlob::Abort,
            on_missing_file: OnMissingFile::SiblingFirst,
        };
        assert_eq!(map_options(&strict), sekai_app::CheckoutOptions::default());

        let lenient = CheckoutFlags {
            keep_post_commit_files: true,
            keep_post_commit_chunks: true,
            keep_tombstoned_chunks: true,
            on_missing_blob: OnMissingBlob::SkipChunk,
            on_missing_file: OnMissingFile::Error,
        };
        assert_eq!(
            map_options(&lenient),
            sekai_app::CheckoutOptions {
                keep_post_commit_files: true,
                keep_post_commit_chunks: true,
                keep_tombstoned_chunks: true,
                on_missing_blob: sekai_app::MissingBlobPolicy::SkipChunk,
                on_missing_file: sekai_app::MissingFilePolicy::Error,
            }
        );
    }

    #[test]
    fn summarizes_restore_policy() {
        let strict = CheckoutFlags {
            keep_post_commit_files: false,
            keep_post_commit_chunks: false,
            keep_tombstoned_chunks: false,
            on_missing_blob: OnMissingBlob::Abort,
            on_missing_file: OnMissingFile::SiblingFirst,
        };
        assert_eq!(policy_summary(&strict), "strict");

        let lenient = CheckoutFlags {
            keep_post_commit_files: true,
            keep_post_commit_chunks: false,
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
