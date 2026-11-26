//! Subcommand execution dispatcher and output control wrappers.

mod branch;
mod cache;
mod checkout;
mod commit;
mod diff;
mod diff_render;
mod init;
mod log;
mod progress;
mod repo;
mod restore;
mod scopes;
mod status;

use anyhow::Context as _;
use sekai_app::{Clock, Scope, SekaiInstance, SekaiStore, ThreadRunner};

use crate::cli::{Cli, Command, Selection, TimingArgs};
use crate::style::Styler;

/// Render Unix milliseconds as RFC 3339, falling back to raw millis when
/// the timestamp does not fit.
fn format_time(created_at_ms: u64) -> String {
    i64::try_from(created_at_ms)
        .ok()
        .and_then(|millis| jiff::Timestamp::from_millisecond(millis).ok())
        .map_or_else(
            || format!("{created_at_ms}ms"),
            |time| time.display_with_offset(jiff::tz::Offset::UTC).to_string(),
        )
}

/// The scope a selection stands for: `--scope NAME` looked up in the
/// store, or the inline `--in`/`--region`/`--kind` spelling.
async fn resolve_selection<S: SekaiStore, T: ThreadRunner, B: Clock>(
    instance: &SekaiInstance<S, T, B>,
    selection: &Selection,
) -> anyhow::Result<Scope> {
    match selection {
        Selection::Inline(inline) => Ok(inline.owned_scope()),
        Selection::Scope(name) => instance
            .resolve_scope(name)
            .await
            .with_context(|| format!("scope {name} failed to resolve")),
    }
}

/// Output controls shared by report commands (commit, status, checkout).
#[derive(Debug, Clone, Copy)]
pub struct ReportOut {
    pub timing: bool,
    pub json: bool,
    pub progress: bool,
    pub style: Styler,
}

impl ReportOut {
    const fn of(output: TimingArgs, style: Styler) -> Self {
        Self {
            timing: output.timing,
            json: output.json,
            progress: output.progress,
            style,
        }
    }
}

/// Execute the top-level command specified in CLI arguments.
pub async fn run(cli: &Cli) -> anyhow::Result<()> {
    let style = Styler::new(cli.color);
    match &cli.command {
        Command::Init { json } => init::run(&cli.store, *json, style).await,
        Command::Commit {
            world,
            output,
            jobs,
            no_cache,
            selection,
        } => {
            commit::run(
                &cli.store,
                world,
                *jobs,
                *no_cache,
                selection,
                ReportOut::of(*output, style),
            )
            .await
        }
        Command::Status {
            world,
            output,
            selection,
            jobs,
        } => {
            status::run(
                &cli.store,
                world,
                *jobs,
                selection,
                ReportOut::of(*output, style),
            )
            .await
        }
        Command::Checkout {
            world,
            commit,
            output,
            jobs,
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
                selection,
                &checkout::CheckoutFlags {
                    keep_post_commit_files: *keep_post_commit_files,
                    keep_post_commit_chunks: *keep_post_commit_chunks,
                    keep_tombstoned_chunks: *keep_tombstoned_chunks,
                    on_missing_blob: *on_missing_blob,
                    on_missing_file: *on_missing_file,
                    jobs: *jobs,
                },
                ReportOut::of(*output, style),
            )
            .await
        }
        Command::Log { json, stat } => log::run(&cli.store, *json, *stat, style).await,
        Command::Scope { action } => scopes::run(&cli.store, action, style).await,
        Command::Cache { action } => cache::run(&cli.store, action, style).await,
        Command::Diff(args) => diff::run(&cli.store, args, style).await,
        Command::Restore(args) => restore::run(&cli.store, args, style).await,
        Command::Branch(args) => branch::run(&cli.store, args, style).await,
        Command::Repo { action } => repo::run(action, style).await,
    }
}
