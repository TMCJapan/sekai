//! Checkout subcommand execution, DTOs, and output rendering
//! (ADR-0036 *checkout*).

use sekai_app::{
    CheckoutReport, CheckoutTimings, HeadMove, HeadState, Location, Restored, Switched,
};
use serde::Serialize;

use super::progress::Bars;
use super::{HeadJson, ReportOut, resolve_selection};
use crate::cli::{CheckoutArgs, OnMissingBlob, OnMissingFile};
use crate::envelope::Printer;
use crate::style::Styler;

const fn map_options(args: &CheckoutArgs) -> sekai_app::CheckoutOptions {
    sekai_app::CheckoutOptions {
        keep_post_commit_files: args.keep_post_commit_files,
        keep_post_commit_chunks: args.keep_post_commit_chunks,
        keep_tombstoned_chunks: args.keep_tombstoned_chunks,
        on_missing_blob: match args.on_missing_blob {
            OnMissingBlob::Abort => sekai_app::MissingBlobPolicy::Abort,
            OnMissingBlob::SkipChunk => sekai_app::MissingBlobPolicy::SkipChunk,
        },
        on_missing_file: match args.on_missing_file {
            OnMissingFile::SiblingFirst => sekai_app::MissingFilePolicy::SiblingFirst,
            OnMissingFile::DerivedOnly => sekai_app::MissingFilePolicy::DerivedOnly,
            OnMissingFile::Error => sekai_app::MissingFilePolicy::Error,
        },
        concurrency: args.jobs,
    }
}

pub async fn run(
    location: &Location,
    args: &CheckoutArgs,
    printer: &Printer,
) -> anyhow::Result<()> {
    let mut instance = sekai_app::SekaiInstance::open_location(location).await?;
    let mut world = location.world_tree()?;
    let out = ReportOut::of(args.output, printer.style);
    let options = map_options(args);
    let bars = Bars::new(out.progress);

    // What the run rebuilt, if anything: `checkout` alone and `HEAD`
    // without `-f` change nothing, as in Git.
    let rebuilt = if args.selection.is_given() {
        let scope = resolve_selection(&instance, &args.selection).await?;
        let restored = instance
            .world_mut(&mut world)
            .restore_from(args.target.as_ref(), scope, args.force, options, &bars)
            .await?;
        Some(restored)
    } else if let Some(target) = &args.target {
        let switched = instance
            .world_mut(&mut world)
            .switch_to(target, args.force, options, &bars)
            .await?;
        if !out.json {
            print_switch(&switched, target.as_str());
        }
        (switched.target.head != HeadMove::Keep || args.force).then_some(Restored {
            commit: switched.target.commit,
            report: switched.report,
            timings: switched.timings,
        })
    } else {
        instance
            .world_mut(&mut world)
            .checkout_head(args.force, options, &bars)
            .await?
    };
    let head = instance.head().await?;
    print_report(rebuilt.as_ref(), head.as_ref(), out, printer)
}

/// Git's messages for a switch, on stderr as Git prints them:
/// `Switched to branch '<b>'`, `Already on '<b>'`, or the detached HEAD
/// advice followed by `HEAD is now at <id>`. `HEAD` itself prints nothing.
fn print_switch(switched: &Switched, written: &str) {
    match &switched.target.head {
        HeadMove::Keep => {}
        HeadMove::Attach(branch) => {
            if switched.from.branch() == Some(branch) {
                eprintln!("Already on '{branch}'");
            } else {
                eprintln!("Switched to branch '{branch}'");
            }
        }
        HeadMove::Detach { .. } => {
            // Git advises only when HEAD leaves a branch.
            if !matches!(switched.from, HeadState::Detached { .. }) {
                eprint!("{}", detached_advice(written));
            }
            eprintln!("{}", super::head_now_at(&switched.target.commit));
        }
    }
}

/// Git's detached HEAD advice, with `sekai branch` and `sekai checkout`
/// in place of `git switch -c`, and without the `git switch -` and
/// `advice.detachedHead` lines, which sekai has no counterpart for
/// (ADR-0036 *checkout*).
fn detached_advice(written: &str) -> String {
    format!(
        "Note: switching to '{written}'.

You are in 'detached HEAD' state. You can look around, make experimental
changes and commit them, and you can discard any commits you make in this
state without impacting any branches by switching back to a branch.

If you want to create a new branch to retain commits you create, you may
do so (now or later) by creating a branch and checking it out. Example:

  sekai branch <new-branch-name>
  sekai checkout <new-branch-name>

"
    )
}

/// sekai's report of a rebuild, after Git's messages. JSON always gets one
/// envelope, with zero counts when nothing was rebuilt.
fn print_report(
    rebuilt: Option<&Restored>,
    head: Option<&HeadState>,
    out: ReportOut,
    printer: &Printer,
) -> anyhow::Result<()> {
    if out.json {
        let (report, timings) = rebuilt.map_or_else(
            || (CheckoutReport::default(), CheckoutTimings::default()),
            |restored| (restored.report, restored.timings.clone()),
        );
        let payload = checkout_payload(&report, &timings, head, out.timing);
        printer.print_ok("checkout", &payload)?;
        return Ok(());
    }
    let Some(restored) = rebuilt else {
        return Ok(());
    };
    let style = out.style;
    let report = &restored.report;
    println!(
        "commit {} restored: {} files rewritten, {} files deleted, {} chunks restored",
        style.bold(&restored.commit.to_string()),
        report.files_written,
        report.files_deleted,
        report.chunks_restored
    );
    if out.timing {
        print_checkout_timing_table(&restored.timings, style);
    }
    Ok(())
}

#[derive(Serialize)]
struct CheckoutPayload {
    files_written: usize,
    files_deleted: usize,
    chunks_restored: usize,
    /// HEAD after the checkout (ADR-0036 *Output*).
    head: Option<HeadJson>,
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
    head: Option<&HeadState>,
    timing: bool,
) -> CheckoutPayload {
    CheckoutPayload {
        files_written: report.files_written,
        files_deleted: report.files_deleted,
        chunks_restored: report.chunks_restored,
        head: head.map(HeadJson::from),
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
            check: Duration::ZERO,
            checkout_files: Duration::from_millis(70),
        };
        let commit = sekai_app::CommitId([9; 32]);
        let head = HeadState::Detached { commit };
        let payload = checkout_payload(&report, &timings, Some(&head), false);
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            format!(
                r#"{{"files_written":2,"files_deleted":1,"chunks_restored":40,"head":{{"ref":null,"commit":"{commit}"}}}}"#
            )
        );
        let payload_timed = checkout_payload(&report, &timings, None, true);
        assert_eq!(
            serde_json::to_string(&payload_timed).unwrap(),
            r#"{"files_written":2,"files_deleted":1,"chunks_restored":40,"head":null,"total_ms":90,"phases":{"plan_ms":9,"discover_ms":8,"checkout_files_ms":70}}"#
        );
    }
}
