//! Subcommand execution dispatcher and output control wrappers.

mod checkout;
mod clone;
mod commit;
mod diff;
mod diff_render;
mod gc;
mod log;
mod progress;
mod prune;
mod status;
mod tag;

use crate::cli::{Cli, Command, TimingArgs};
use crate::style::Styler;

/// Output controls shared by report commands (commit, status, checkout,
/// clone, prune, gc).
#[derive(Debug, Clone, Copy)]
pub struct ReportOut {
    pub timing: bool,
    pub json: bool,
    pub style: Styler,
}

impl ReportOut {
    const fn of(output: TimingArgs, style: Styler) -> Self {
        Self {
            timing: output.timing,
            json: output.json,
            style,
        }
    }
}

/// Execute the top-level command specified in CLI arguments.
pub async fn run(cli: &Cli) -> anyhow::Result<()> {
    let style = Styler::new(cli.color);
    match &cli.command {
        Command::Commit {
            world,
            output,
            with_diff,
            jobs,
            progress,
            selection,
        } => {
            commit::run(
                &cli.store,
                world,
                *with_diff,
                *jobs,
                *progress,
                selection,
                ReportOut::of(*output, style),
            )
            .await
        }
        Command::Status {
            world,
            output,
            progress,
            selection,
            jobs,
        } => {
            status::run(
                &cli.store,
                world,
                *jobs,
                *progress,
                selection,
                ReportOut::of(*output, style),
            )
            .await
        }
        Command::Checkout {
            world,
            commit,
            output,
            progress,
            selection,
            keep_post_commit_files,
            keep_post_commit_chunks,
            keep_tombstoned_chunks,
            on_missing_blob,
            on_missing_file,
        } => {
            checkout::run(
                &cli.store,
                world,
                commit,
                *progress,
                selection,
                &checkout::CheckoutFlags {
                    keep_post_commit_files: *keep_post_commit_files,
                    keep_post_commit_chunks: *keep_post_commit_chunks,
                    keep_tombstoned_chunks: *keep_tombstoned_chunks,
                    on_missing_blob: *on_missing_blob,
                    on_missing_file: *on_missing_file,
                },
                ReportOut::of(*output, style),
            )
            .await
        }
        Command::Log { json, stat } => log::run(&cli.store, *json, *stat, style).await,
        Command::Tag { action } => tag::run(&cli.store, action, style).await,
        Command::Clone {
            commit,
            out,
            flavor,
            base,
            on_missing_blob,
            selection,
            output,
            progress,
        } => {
            clone::run(
                &cli.store,
                commit,
                out,
                *progress,
                selection,
                &clone::CloneFlags {
                    flavor: *flavor,
                    base: base.clone(),
                    on_missing_blob: *on_missing_blob,
                },
                ReportOut::of(*output, style),
            )
            .await
        }
        Command::Diff(args) => diff::run(&cli.store, args, style).await,
        Command::Prune {
            keep_last,
            before,
            dry_run,
            output,
            progress,
        } => {
            prune::run(
                &cli.store,
                *keep_last,
                before.as_ref(),
                *dry_run,
                *progress,
                ReportOut::of(*output, style),
            )
            .await
        }
        Command::Gc {
            dry_run,
            output,
            progress,
        } => {
            gc::run(
                &cli.store,
                *dry_run,
                *progress,
                ReportOut::of(*output, style),
            )
            .await
        }
    }
}
