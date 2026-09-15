//! Subcommand execution dispatcher and output control wrappers.

mod backup;
mod debug;
mod diff;
mod diff_render;
mod gc;
mod list;
mod rollback;

use crate::cli::{Cli, Command, DebugCommand};
use crate::style::Styler;

/// Output controls shared by report commands (backup, rollback, gc).
#[derive(Debug, Clone, Copy)]
pub struct ReportOut {
    pub timing: bool,
    pub json: bool,
    pub style: Styler,
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
            selection,
        } => {
            let out = ReportOut {
                timing: output.timing,
                json: output.json,
                style,
            };
            backup::run(&cli.store, world, *with_diff, *jobs, selection, out).await
        }
        Command::Rollback {
            world,
            snapshot,
            output,
            selection,
        } => {
            let out = ReportOut {
                timing: output.timing,
                json: output.json,
                style,
            };
            rollback::run(&cli.store, world, *snapshot, selection, out).await
        }
        Command::List { json } => list::run(&cli.store, *json, style).await,
        Command::Diff(args) => diff::run(&cli.store, args, style).await,
        Command::Gc { dry_run, output } => {
            let out = ReportOut {
                timing: output.timing,
                json: output.json,
                style,
            };
            gc::run(&cli.store, *dry_run, out).await
        }
        Command::Debug { debug } => match debug {
            DebugCommand::Scan { world, output } => debug::run_scan(world, *output, style),
        },
    }
}
