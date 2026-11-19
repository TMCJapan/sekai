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
mod reflog;
mod scopes;
mod status;
mod tag;
mod worktree;

use std::path::PathBuf;

use sekai_app::{
    AppError, HeadState, Location, RefKind, RefName, Scope, SekaiInstance, SekaiStore,
};
use serde::Serialize;

use crate::cli::{Cli, Command, Selection, TimingArgs};
use crate::envelope::Printer;
use crate::style::Styler;

/// A branch or tag in JSON (ADR-0036): the full refname and the commit it
/// points at, when it has one.
#[derive(Debug, Serialize)]
pub(super) struct RefJson {
    name: String,
    commit: Option<String>,
}

impl RefJson {
    pub(super) fn new(kind: RefKind, name: &RefName, commit: Option<sekai_app::CommitId>) -> Self {
        Self {
            name: kind.full_name(name),
            commit: commit.as_ref().map(ToString::to_string),
        }
    }

    pub(super) fn branch(name: &RefName, commit: Option<sekai_app::CommitId>) -> Self {
        Self::new(RefKind::Branch, name, commit)
    }

    pub(super) fn tag(name: &RefName, commit: Option<sekai_app::CommitId>) -> Self {
        Self::new(RefKind::Tag, name, commit)
    }
}

/// `HEAD is now at <id>` as Git prints it.
pub(super) fn head_now_at(commit: &impl std::fmt::Display) -> String {
    format!("HEAD is now at {commit}")
}

/// HEAD in JSON (ADR-0036 *Output*): attached has both fields, unborn a
/// null `commit`, and detached a null `ref`.
#[derive(Debug, Serialize)]
struct HeadJson {
    #[serde(rename = "ref")]
    refname: Option<String>,
    commit: Option<String>,
}

impl From<&HeadState> for HeadJson {
    fn from(head: &HeadState) -> Self {
        Self {
            refname: head
                .branch()
                .map(|branch| RefKind::Branch.full_name(branch)),
            commit: head.commit().as_ref().map(ToString::to_string),
        }
    }
}

/// The scope a selection stands for: `--scope NAME` looked up in the
/// store, or the inline `--in`/`--region`/`--kind` spelling.
async fn resolve_selection<S: SekaiStore>(
    instance: &SekaiInstance<S>,
    selection: &Selection,
) -> anyhow::Result<Scope> {
    match selection {
        Selection::Inline(inline) => Ok(inline.owned_scope()),
        Selection::Scope(name) => Ok(instance.resolve_scope(name).await?),
    }
}

/// Where the command runs: discovery from `--world`, or from the current
/// directory, with `--store` overriding the store (ADR-0036).
fn locate(cli: &Cli) -> anyhow::Result<Location> {
    Ok(sekai_app::discover(start_dir(cli)?, cli.store.as_ref())?)
}

/// `--world`, or the current directory without it.
fn start_dir(cli: &Cli) -> anyhow::Result<PathBuf> {
    match &cli.world {
        Some(world) => Ok(world.clone()),
        None => Ok(std::env::current_dir().map_err(AppError::Io)?),
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
            progress: output.progress.show(output.json),
            style,
        }
    }
}

/// Execute the top-level command specified in CLI arguments.
///
/// `printer` says how to print.
pub async fn run(cli: &Cli, printer: &Printer) -> anyhow::Result<()> {
    // `init` registers a world instead of discovering one.
    let location = || locate(cli);
    match &cli.command {
        Command::Init { .. } => init::run(start_dir(cli)?, cli.store.as_deref(), printer).await,
        Command::Commit {
            output,
            jobs,
            no_cache,
            selection,
        } => {
            let out = ReportOut::of(*output, printer.style);
            commit::run(&location()?, *jobs, *no_cache, selection, out, printer).await
        }
        Command::Status {
            output,
            selection,
            jobs,
        } => {
            let out = ReportOut::of(*output, printer.style);
            status::run(&location()?, *jobs, selection, out, printer).await
        }
        Command::Checkout(args) => checkout::run(&location()?, args, printer).await,
        Command::Branch(command) => branch::run(&location()?, command, printer).await,
        Command::Tag(command) => tag::run(&location()?, command, printer).await,
        Command::Log(args) => log::run(&location()?, args, printer).await,
        Command::Reflog(command) => reflog::run(&location()?, command, printer).await,
        Command::Worktree { action } => worktree::run(&location()?, action, printer).await,
        Command::Scope { action } => scopes::run(&location()?, action, printer).await,
        Command::Cache { action } => cache::run(&location()?, action, printer).await,
        Command::Diff(args) => diff::run(&location()?, args, printer).await,
    }
}
