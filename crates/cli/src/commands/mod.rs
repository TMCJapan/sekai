//! Subcommand execution dispatcher and output control wrappers.

mod backup;
mod debug;
mod diff;
mod diff_render;
mod export;
mod gc;
mod list;
mod progress;
mod rollback;
mod status;
mod tag;

use crate::cli::{Cli, Command, DebugCommand, TimingArgs};
use crate::style::Styler;

/// Output controls shared by report commands (backup, rollback, gc).
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
        Command::Backup {
            world,
            output,
            with_diff,
            jobs,
            progress,
            selection,
        } => {
            backup::run(
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
        Command::Rollback {
            world,
            snapshot,
            output,
            progress,
            selection,
            keep_post_snapshot_files,
            keep_post_snapshot_chunks,
            keep_tombstoned_chunks,
            on_missing_blob,
            on_missing_file,
        } => {
            rollback::run(
                &cli.store,
                world,
                snapshot,
                *progress,
                selection,
                &rollback::RollbackFlags {
                    keep_post_snapshot_files: *keep_post_snapshot_files,
                    keep_post_snapshot_chunks: *keep_post_snapshot_chunks,
                    keep_tombstoned_chunks: *keep_tombstoned_chunks,
                    on_missing_blob: *on_missing_blob,
                    on_missing_file: *on_missing_file,
                },
                ReportOut::of(*output, style),
            )
            .await
        }
        Command::List { json, stat } => list::run(&cli.store, *json, *stat, style).await,
        Command::Tag {
            name,
            snapshot,
            delete,
            force,
            json,
        } => {
            tag::run(
                &cli.store,
                name.clone(),
                snapshot.clone(),
                *delete,
                *force,
                *json,
                style,
            )
            .await
        }
        Command::Export {
            snapshot,
            out,
            flavor,
            base,
            on_missing_blob,
            selection,
            output,
            progress,
        } => {
            export::run(
                &cli.store,
                snapshot,
                out,
                *progress,
                selection,
                &export::ExportFlags {
                    flavor: *flavor,
                    base: base.clone(),
                    on_missing_blob: *on_missing_blob,
                },
                ReportOut::of(*output, style),
            )
            .await
        }
        Command::Diff(args) => diff::run(&cli.store, args, style).await,
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
        Command::Debug { debug } => match debug {
            DebugCommand::Scan { world, output } => debug::run_scan(world, *output, style),
        },
    }
}
