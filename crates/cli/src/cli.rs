//! Command-line argument parsing and selection structures.

use crate::style::ColorChoice;
use clap::{Parser, Subcommand};
use sekai_app::{
    Area, ChunkCoord, Dimension, Rect, RefName, ReflogSelector, RegionKind, Revision,
    RevisionRange, Scope, ScopeName,
};
use std::path::PathBuf;

/// Chunk-level deduplicated version control for Minecraft region files.
#[derive(Debug, Parser)]
#[command(name = "sekai", version, about)]
pub struct Cli {
    /// Start discovery at this directory instead of the current one: the
    /// first `.sekai` from here up names the store and the worktree. `init`
    /// registers this directory as a world (ADR-0036).
    #[arg(long, global = true, value_name = "WORLD")]
    pub world: Option<PathBuf>,

    /// Store directory, overriding the one discovery finds. When the
    /// world's `.sekai` is another store, the command runs outside every
    /// worktree. `init` defaults it to the world's `.sekai`.
    #[arg(long, global = true, value_name = "STORE")]
    pub store: Option<PathBuf>,

    /// Colorize human output. JSON output is never colorized.
    #[arg(long, global = true, value_enum, default_value_t = ColorChoice::Auto)]
    pub color: ColorChoice,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Register a world in a store, creating the store when it is
    /// missing. Every other command requires a store created this way.
    Init {
        /// Emit output as JSON instead of human text. See docs/json.md.
        #[arg(long)]
        json: bool,
    },
    /// Record the current world state as a new commit.
    Commit {
        /// Human or JSON rendering plus optional phase timings.
        #[command(flatten)]
        output: TimingArgs,
        /// Ingest worker count. `0` means one per CPU.
        #[arg(short, long, default_value = "0")]
        jobs: usize,
        /// Skip the pre-commit cache update pass (ADR-0034): record
        /// history only, leaving the region-image cache untouched.
        #[arg(long)]
        no_cache: bool,
        /// World portion to record (default: whole world).
        #[command(flatten)]
        selection: Selection,
    },
    /// Preview what a commit would record, without writing anything.
    Status {
        /// Human or JSON rendering plus optional phase timings.
        #[command(flatten)]
        output: TimingArgs,
        /// World portion to preview (default: whole world).
        #[command(flatten)]
        selection: Selection,
        /// Preview worker count. `0` means one per CPU.
        #[arg(short, long, default_value = "0")]
        jobs: usize,
    },
    /// Switch HEAD to a branch or a commit, or restore an area of the
    /// world from a commit.
    Checkout(CheckoutArgs),
    /// List, create, delete, or rename branches.
    Branch(BranchCommand),
    /// List, create, or delete lightweight tags.
    Tag(TagCommand),
    /// Walk the history from HEAD or the given revisions.
    Log(LogArgs),
    /// Show the log of HEAD or a branch.
    Reflog(ReflogCommand),
    /// Add, list, or remove worktrees.
    Worktree {
        #[command(subcommand)]
        action: WorktreeCommand,
    },
    /// Manage named scopes: store-saved world areas selected with `--scope`.
    Scope {
        #[command(subcommand)]
        action: ScopeCommand,
    },
    /// Manage the rebuildable region-image cache.
    Cache {
        #[command(subcommand)]
        action: CacheCommand,
    },
    /// Compare chunks between the world and a commit, or between two
    /// commits.
    Diff(DiffArgs),
}

impl Command {
    /// Stable command name for JSON envelopes and error objects.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Init { .. } => "init",
            Self::Commit { .. } => "commit",
            Self::Status { .. } => "status",
            Self::Checkout(_) => "checkout",
            Self::Branch(_) => "branch",
            Self::Tag(_) => "tag",
            Self::Log(_) => "log",
            Self::Reflog(_) => "reflog",
            Self::Worktree { .. } => "worktree",
            Self::Scope { .. } => "scope",
            Self::Cache { .. } => "cache",
            Self::Diff(_) => "diff",
        }
    }

    /// Whether the command runs in JSON mode (`--json`). Errors serialize as
    /// an envelope on stdout instead of styled text on stderr.
    pub const fn output_json(&self) -> bool {
        match self {
            Self::Commit { output, .. } | Self::Status { output, .. } => output.json,
            Self::Checkout(args) => args.output.json,
            Self::Init { json } => *json,
            Self::Branch(command) => command.json,
            Self::Tag(command) => command.json,
            Self::Log(args) => args.json,
            Self::Reflog(command) => command.json,
            Self::Worktree { action } => action.output_json(),
            Self::Scope { action } => action.output_json(),
            Self::Cache { action } => action.output_json(),
            Self::Diff(args) => args.output.json,
        }
    }
}

/// `checkout [-f] [<branch> | <commit-ish>]` and `checkout [-f]
/// [<commit-ish>] <selection>` (ADR-0036 *checkout*).
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is a command-line flag"
)]
#[derive(Debug, clap::Args)]
pub struct CheckoutArgs {
    /// Branch to switch to, or commit-ish to detach HEAD at. With a
    /// selection, the commit-ish to restore the area from (default: HEAD).
    pub target: Option<Revision>,
    /// Discard uncommitted changes instead of refusing to overwrite them.
    #[arg(short, long)]
    pub force: bool,
    /// Area to restore without moving HEAD. Files outside it are never
    /// written, deleted, or otherwise touched.
    #[command(flatten)]
    pub selection: Selection,
    /// Human or JSON rendering plus optional phase timings.
    #[command(flatten)]
    pub output: TimingArgs,
    /// Rebuild worker count. `0` means one per CPU.
    #[arg(short, long, default_value = "0")]
    pub jobs: usize,
    /// Keep region files unknown to the commit instead of deleting
    /// them (files created after the commit).
    #[arg(long)]
    pub keep_post_commit_files: bool,
    /// Keep live bytes for chunks unknown to the commit (created
    /// afterwards inside a commit-known region) instead of dropping
    /// them.
    #[arg(long)]
    pub keep_post_commit_chunks: bool,
    /// Keep live bytes for commit-tombstoned chunks instead of
    /// removing them. Fully tombstoned region files are left alone
    /// rather than deleted.
    #[arg(long)]
    pub keep_tombstoned_chunks: bool,
    /// How to handle a commit blob missing from CAS.
    #[arg(long, value_enum, default_value_t = OnMissingBlob::Abort)]
    pub on_missing_blob: OnMissingBlob,
    /// Where to rebuild a commit-known region whose file is missing
    /// on disk.
    #[arg(long, value_enum, default_value_t = OnMissingFile::SiblingFirst)]
    pub on_missing_file: OnMissingFile,
}

/// What `branch` does: Git's mode options pick one, and parsing refuses
/// any combination of them (ADR-0036 *branch*).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchAction {
    /// `branch [--list] [<pattern>...]`.
    List { patterns: Vec<String> },
    /// `branch [-f] <name> [<start>]`.
    Create {
        name: RefName,
        start: Option<Revision>,
        force: bool,
    },
    /// `branch (-d | -D) <name>...`.
    Delete { names: Vec<RefName>, force: bool },
    /// `branch (-m | -M) [<old>] <new>`.
    Rename {
        old: Option<RefName>,
        new: RefName,
        force: bool,
    },
    /// `branch --show-current`.
    ShowCurrent,
}

/// `branch` with its mode resolved.
#[derive(Debug, Clone)]
pub struct BranchCommand {
    pub action: BranchAction,
    pub json: bool,
}

/// The `branch` flags as clap parses them, before the mode decides what
/// the positionals mean.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is a command-line flag"
)]
#[derive(Debug, Clone, clap::Args)]
#[command(group(
    clap::ArgGroup::new("mode")
        .args(["list", "delete", "force_delete", "rename", "force_rename", "show_current"])
        .multiple(false)
))]
struct BranchArgs {
    /// List branches, filtered by glob patterns (the default without a
    /// name).
    #[arg(short, long)]
    list: bool,
    /// Delete branches that HEAD reaches.
    #[arg(short, long)]
    delete: bool,
    /// Delete branches even when HEAD does not reach them.
    #[arg(short = 'D')]
    force_delete: bool,
    /// Rename a branch, or HEAD's branch when only the new name is given.
    #[arg(short = 'm', long = "move")]
    rename: bool,
    /// Rename a branch over an existing one.
    #[arg(short = 'M')]
    force_rename: bool,
    /// Print HEAD's branch, or nothing when HEAD is detached.
    #[arg(long)]
    show_current: bool,
    /// Move an existing branch to the start point.
    #[arg(short, long, conflicts_with_all = ["list", "delete", "force_delete", "rename", "force_rename", "show_current"])]
    force: bool,
    /// Names, a name and a start point, or patterns, as the mode reads them.
    args: Vec<String>,
    /// Emit output as JSON instead of human text. See docs/json.md.
    #[arg(long)]
    json: bool,
}

impl BranchArgs {
    fn into_command(self) -> Result<BranchCommand, clap::Error> {
        let mut args = self.args.into_iter();
        let action = if self.show_current {
            if args.next().is_some() {
                return Err(UsageError::TooManyArguments(Extra::ShowCurrent).into());
            }
            BranchAction::ShowCurrent
        } else if self.delete || self.force_delete {
            let names = args
                .map(|arg| ref_name(&arg))
                .collect::<Result<Vec<_>, _>>()?;
            if names.is_empty() {
                return Err(UsageError::NameRequired(Named::Branch).into());
            }
            BranchAction::Delete {
                names,
                force: self.force_delete,
            }
        } else if self.rename || self.force_rename {
            let (old, new) = match (args.next(), args.next(), args.next()) {
                (Some(new), None, None) => (None, new),
                (Some(old), Some(new), None) => (Some(ref_name(&old)?), new),
                (None, ..) => return Err(UsageError::NameRequired(Named::Branch).into()),
                _ => return Err(UsageError::TooManyArguments(Extra::Rename).into()),
            };
            BranchAction::Rename {
                old,
                new: ref_name(&new)?,
                force: self.force_rename,
            }
        } else if self.list || args.len() == 0 {
            BranchAction::List {
                patterns: args.collect(),
            }
        } else {
            let (Some(name), start, None) = (args.next(), args.next(), args.next()) else {
                return Err(UsageError::TooManyArguments(Extra::Any).into());
            };
            BranchAction::Create {
                name: ref_name(&name)?,
                start: start.map(|start| revision(&start)).transpose()?,
                force: self.force,
            }
        };
        Ok(BranchCommand {
            action,
            json: self.json,
        })
    }
}

/// What `tag` does (ADR-0036 *tag*).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagAction {
    /// `tag [-l] [<pattern>...]`.
    List { patterns: Vec<String> },
    /// `tag [-f] <name> [<commit-ish>]`.
    Create {
        name: RefName,
        target: Option<Revision>,
        force: bool,
    },
    /// `tag -d <name>...`.
    Delete { names: Vec<RefName> },
}

/// `tag` with its mode resolved.
#[derive(Debug, Clone)]
pub struct TagCommand {
    pub action: TagAction,
    pub json: bool,
}

/// The `tag` flags as clap parses them, before the mode decides what the
/// positionals mean.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is a command-line flag"
)]
#[derive(Debug, Clone, clap::Args)]
struct TagArgs {
    /// List tags, filtered by glob patterns (the default without a name).
    #[arg(short, long, conflicts_with = "delete")]
    list: bool,
    /// Delete tags.
    #[arg(short, long)]
    delete: bool,
    /// Replace an existing tag.
    #[arg(short, long, conflicts_with_all = ["list", "delete"])]
    force: bool,
    /// Names, a name and a commit-ish, or patterns, as the mode reads them.
    args: Vec<String>,
    /// Emit output as JSON instead of human text. See docs/json.md.
    #[arg(long)]
    json: bool,
}

impl TagArgs {
    fn into_command(self) -> Result<TagCommand, clap::Error> {
        let mut args = self.args.into_iter();
        let action = if self.delete {
            let names = args
                .map(|arg| ref_name(&arg))
                .collect::<Result<Vec<_>, _>>()?;
            if names.is_empty() {
                return Err(UsageError::NameRequired(Named::Tag).into());
            }
            TagAction::Delete { names }
        } else if self.list || args.len() == 0 {
            TagAction::List {
                patterns: args.collect(),
            }
        } else {
            let (Some(name), target, None) = (args.next(), args.next(), args.next()) else {
                return Err(UsageError::TooManyArguments(Extra::Any).into());
            };
            TagAction::Create {
                name: ref_name(&name)?,
                target: target.map(|target| revision(&target)).transpose()?,
                force: self.force,
            }
        };
        Ok(TagCommand {
            action,
            json: self.json,
        })
    }
}

/// `log` (ADR-0036 *log*).
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is a command-line flag"
)]
#[derive(Debug, clap::Args)]
pub struct LogArgs {
    /// Start from every ref and every detached HEAD as well.
    #[arg(long)]
    pub all: bool,
    /// List the oldest commit first.
    #[arg(long)]
    pub reverse: bool,
    /// Print each commit on one line.
    #[arg(long)]
    pub oneline: bool,
    /// Print the refs that point at each commit (default when stdout is
    /// a terminal).
    #[arg(long, overrides_with = "no_decorate")]
    pub decorate: bool,
    /// Never print the refs that point at each commit.
    #[arg(long, overrides_with = "decorate")]
    pub no_decorate: bool,
    /// Include per-commit change statistics.
    #[arg(long)]
    pub stat: bool,
    /// Revisions to start from (`<rev>`), to leave out (`^<rev>`), or a
    /// range (`<a>..<b>`).
    #[arg(value_name = "REVISION-RANGE", allow_hyphen_values = false)]
    pub ranges: Vec<RevisionRange>,
    /// Emit the commit list as JSON instead of human text. See
    /// docs/json.md.
    #[arg(long)]
    pub json: bool,
}

/// `reflog [show] [<ref>]` with its arguments read.
#[derive(Debug, Clone)]
pub struct ReflogCommand {
    /// Whose log to show: `HEAD` or a branch.
    pub log: ReflogSelector,
    pub json: bool,
}

/// The `reflog` arguments as clap parses them.
#[derive(Debug, Clone, clap::Args)]
struct ReflogArgs {
    /// `show`, then `HEAD` or a branch (default: `HEAD`).
    #[arg(num_args = 0..=2)]
    args: Vec<String>,
    /// Emit output as JSON instead of human text. See docs/json.md.
    #[arg(long)]
    json: bool,
}

impl ReflogArgs {
    fn into_command(self) -> Result<ReflogCommand, clap::Error> {
        let mut args = self.args.as_slice();
        // As in Git, a first word `show` is the subcommand, not a ref.
        if let [first, rest @ ..] = args
            && first == "show"
        {
            args = rest;
        }
        let log = match args {
            [] => ReflogSelector::Head,
            [name] if name == "HEAD" => ReflogSelector::Head,
            [name] => ReflogSelector::Ref(ref_name(name)?),
            _ => return Err(UsageError::TooManyArguments(Extra::Any).into()),
        };
        Ok(ReflogCommand {
            log,
            json: self.json,
        })
    }
}

/// Worktree operations (ADR-0035).
#[derive(Debug, Subcommand)]
pub enum WorktreeCommand {
    /// Copy this worktree to a new path and check a commit out there.
    Add {
        /// Directory to create the worktree in; it must be missing or
        /// empty.
        path: PathBuf,
        /// Branch to check out, or commit-ish to detach HEAD at (default:
        /// a new branch named after the directory).
        commit_ish: Option<Revision>,
        /// Progress on stderr.
        #[command(flatten)]
        progress: ProgressArgs,
        /// Emit output as JSON instead of human text. See docs/json.md.
        #[arg(long)]
        json: bool,
    },
    /// List every worktree of the store.
    List {
        /// Emit output as JSON instead of human text. See docs/json.md.
        #[arg(long)]
        json: bool,
    },
    /// Unregister a linked worktree and delete its directory.
    Remove {
        /// The worktree's path, or a unique suffix of it.
        worktree: String,
        /// Emit output as JSON instead of human text. See docs/json.md.
        #[arg(long)]
        json: bool,
    },
}

impl WorktreeCommand {
    /// Whether the operation runs in JSON mode (`--json`).
    pub const fn output_json(&self) -> bool {
        match self {
            Self::Add { json, .. } | Self::List { json } | Self::Remove { json, .. } => *json,
        }
    }
}

/// What parsing refuses once a mode option decides what the positionals
/// mean, worded in one place (ADR-0013).
#[derive(Debug)]
enum UsageError {
    /// The mode needs a name and got none.
    NameRequired(Named),
    /// More positionals than the mode takes.
    TooManyArguments(Extra),
    /// `diff` revisions other than up to two commit-ishes or one range.
    DiffShape,
    /// A positional that is not a ref name.
    InvalidRefName {
        raw: String,
        reason: sekai_app::RefNameError,
    },
    /// A positional that is not a revision.
    InvalidRevision {
        raw: String,
        reason: sekai_app::RevisionError,
    },
}

/// Whose name a mode needs.
#[derive(Debug, Clone, Copy)]
enum Named {
    Branch,
    Tag,
}

/// Which mode got more positionals than it takes; Git words some apart.
#[derive(Debug, Clone, Copy)]
enum Extra {
    Any,
    Rename,
    ShowCurrent,
}

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NameRequired(Named::Branch) => f.write_str("branch name required"),
            Self::NameRequired(Named::Tag) => f.write_str("tag name required"),
            Self::TooManyArguments(Extra::Any) => f.write_str("too many arguments"),
            Self::TooManyArguments(Extra::Rename) => {
                f.write_str("too many arguments for a rename operation")
            }
            Self::TooManyArguments(Extra::ShowCurrent) => {
                f.write_str("--show-current takes no arguments")
            }
            Self::DiffShape => {
                f.write_str("diff takes at most two commit-ishes, or one range <a>..<b>")
            }
            Self::InvalidRefName { raw, reason } => write!(f, "invalid ref name '{raw}': {reason}"),
            Self::InvalidRevision { raw, reason } => {
                write!(f, "invalid revision '{raw}': {reason}")
            }
        }
    }
}

impl From<UsageError> for clap::Error {
    fn from(err: UsageError) -> Self {
        let kind = match err {
            UsageError::NameRequired(_)
            | UsageError::TooManyArguments(_)
            | UsageError::DiffShape => clap::error::ErrorKind::ArgumentConflict,
            UsageError::InvalidRefName { .. } | UsageError::InvalidRevision { .. } => {
                clap::error::ErrorKind::ValueValidation
            }
        };
        Self::raw(kind, format!("{err}\n"))
    }
}

fn ref_name(raw: &str) -> Result<RefName, UsageError> {
    RefName::parse(raw).map_err(|reason| UsageError::InvalidRefName {
        raw: raw.to_owned(),
        reason,
    })
}

fn revision(raw: &str) -> Result<Revision, UsageError> {
    Revision::parse(raw).map_err(|reason| UsageError::InvalidRevision {
        raw: raw.to_owned(),
        reason,
    })
}

/// Commands whose positionals mean what a mode option says: clap parses
/// the raw flags, and the conversion refuses what the mode does not take,
/// still at parse time (ADR-0013).
macro_rules! parsed_through {
    ($command:ty, $raw:ty) => {
        impl clap::FromArgMatches for $command {
            fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
                <$raw>::from_arg_matches(matches)?.into_command()
            }

            fn update_from_arg_matches(
                &mut self,
                matches: &clap::ArgMatches,
            ) -> Result<(), clap::Error> {
                *self = Self::from_arg_matches(matches)?;
                Ok(())
            }
        }

        impl clap::Args for $command {
            fn group_id() -> Option<clap::Id> {
                <$raw>::group_id()
            }

            fn augment_args(cmd: clap::Command) -> clap::Command {
                <$raw>::augment_args(cmd)
            }

            fn augment_args_for_update(cmd: clap::Command) -> clap::Command {
                <$raw>::augment_args_for_update(cmd)
            }
        }
    };
}

parsed_through!(BranchCommand, BranchArgs);
parsed_through!(TagCommand, TagArgs);
parsed_through!(ReflogCommand, ReflogArgs);

/// Named scope operations (ADR-0021). A scope covers every region family,
/// so `create` takes areas but no `--kind`.
#[derive(Debug, Subcommand)]
pub enum ScopeCommand {
    /// Save one or more areas under a name.
    Create {
        /// Scope name (ASCII alphanumerics plus ".", "_", "-", "/"; 1-255
        /// bytes).
        name: ScopeName,
        /// Chunk `DIM:x,z` or rectangle `DIM:x0,z0..x1,z1` (inclusive). A
        /// bare `DIM` is refused: a scope area is a chunk rectangle.
        /// Repeatable, additive with `--region`.
        #[arg(
            long = "in",
            value_name = "DIM:X,Z|X0,Z0..X1,Z1",
            allow_hyphen_values = true,
            required_unless_present = "region"
        )]
        areas: Vec<ScopeAreaSpec>,
        /// Region `DIM:RX,RZ`: every chunk of the file. Repeatable,
        /// additive with `--in`.
        #[arg(long, value_name = "DIM:RX,RZ", allow_hyphen_values = true)]
        region: Vec<RegionSpec>,
        /// Replace an existing scope instead of failing.
        #[arg(long)]
        force: bool,
        /// Emit output as JSON instead of human text. See docs/json.md.
        #[arg(long)]
        json: bool,
    },
    /// Delete a scope.
    Delete {
        /// Scope name to delete.
        name: ScopeName,
        /// Emit output as JSON instead of human text. See docs/json.md.
        #[arg(long)]
        json: bool,
    },
    /// List all scopes in name order.
    List {
        /// Emit output as JSON instead of human text. See docs/json.md.
        #[arg(long)]
        json: bool,
    },
}

impl ScopeCommand {
    /// Whether the operation runs in JSON mode (`--json`).
    pub const fn output_json(&self) -> bool {
        match self {
            Self::Create { json, .. } | Self::Delete { json, .. } | Self::List { json } => *json,
        }
    }
}

/// Cache operations (ADR-0029). The cache is derived state: every
/// operation here is safe at any time and costs performance only, never
/// changing what a commit or checkout reads or writes.
#[derive(Debug, Subcommand)]
pub enum CacheCommand {
    /// Remove every cached region image to reclaim space. The cache
    /// rebuilds itself on later commits; behavior is unchanged.
    Clean {
        /// Emit output as JSON instead of human text. See docs/json.md.
        #[arg(long)]
        json: bool,
    },
    /// Hash the live world's region files and populate the cache without
    /// recording a commit. Reads and hashes every file; writes only cache
    /// entries, never the CAS or metadata.
    Update {
        /// Human or JSON rendering plus optional phase timings.
        #[command(flatten)]
        output: TimingArgs,
        /// Hashing worker count. `0` means one per CPU.
        #[arg(short, long, default_value = "0")]
        jobs: usize,
    },
}

impl CacheCommand {
    /// Whether the operation runs in JSON mode (`--json`).
    pub const fn output_json(&self) -> bool {
        match self {
            Self::Clean { json } => *json,
            Self::Update { output, .. } => output.json,
        }
    }
}

/// `--timing`/`--json`/`--[no-]progress` flags shared by every report
/// command except `log` (which has no phases to time or items to count).
#[derive(Debug, Clone, Copy, clap::Args)]
pub struct TimingArgs {
    /// Print a per-phase timing breakdown after the report.
    #[arg(long)]
    pub timing: bool,
    /// Emit output as JSON instead of human text, with progress off.
    /// Combined with `--timing`, phase timings are included. See
    /// docs/json.md.
    #[arg(long)]
    pub json: bool,
    /// Progress on stderr.
    #[command(flatten)]
    pub progress: ProgressArgs,
}

/// `--progress` and `--no-progress`, as Git spells them: progress shows by
/// default, drawn only when stderr is a terminal (ADR-0036 *Options*).
#[derive(Debug, Clone, Copy, clap::Args)]
pub struct ProgressArgs {
    /// Show progress on stderr (the default). Refused with `--json`.
    #[arg(long, overrides_with = "no_progress", conflicts_with = "json")]
    progress: bool,
    /// Never show progress.
    #[arg(long, overrides_with = "progress")]
    no_progress: bool,
}

impl ProgressArgs {
    /// Whether to show progress: always but with `--no-progress`, and never
    /// with `--json`, which keeps stderr silent (ADR-0002).
    pub const fn show(self, json: bool) -> bool {
        !json && !self.no_progress
    }
}

/// What to do when a commit blob is absent from CAS.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum OnMissingBlob {
    /// Abort loudly (a missing blob is corruption).
    Abort,
    /// Skip the chunk, leaving it out of the rebuilt file.
    SkipChunk,
}

/// Where to rebuild a commit-known region whose file is missing on disk.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum OnMissingFile {
    /// Prefer a same-dimension sibling's directory, then the derived path.
    SiblingFirst,
    /// Always use the layout-derived path.
    DerivedOnly,
    /// Fail loudly instead of guessing a location.
    Error,
}

/// `diff [<selection>] [<commit-ish> [<commit-ish>]]` and `diff
/// [<selection>] <a>..<b>`, with the revisions read (ADR-0036 *diff*).
#[derive(Debug)]
pub struct DiffArgs {
    /// What the invocation compares.
    pub target: DiffTarget,
    /// Chunks to compare (default: whole world).
    pub selection: Selection,
    /// Human or JSON rendering plus optional phase timings.
    pub output: TimingArgs,
    /// Accepted for compatibility; has no effect.
    pub show_values: bool,
}

/// Comparison sides of a `diff` run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffTarget {
    /// The world against one commit (`None` means HEAD's commit).
    World(Option<Revision>),
    /// Commit against commit.
    Commits { old: Revision, new: Revision },
}

/// The `diff` arguments as clap parses them, before the revisions decide
/// what the invocation compares.
#[derive(Debug, clap::Args)]
struct RawDiffArgs {
    /// Commit-ishes: none or one compares the world with HEAD or that
    /// commit; two, or `<a>..<b>`, compares the two commits.
    #[arg(value_name = "COMMIT-ISH")]
    revisions: Vec<RevisionRange>,
    /// Chunks to compare (default: whole world). Only chunks with
    /// differences are reported.
    #[command(flatten)]
    selection: Selection,
    /// Human or JSON rendering plus optional phase timings.
    #[command(flatten)]
    output: TimingArgs,
    /// Accepted for compatibility; the chunk diff carries normalized
    /// hashes, not per-tag values, so this flag has no effect.
    #[arg(long)]
    show_values: bool,
}

impl RawDiffArgs {
    fn into_command(self) -> Result<DiffArgs, clap::Error> {
        use RevisionRange::{Between, Include};
        let mut revisions = self.revisions.into_iter();
        let target = match (revisions.next(), revisions.next(), revisions.next()) {
            (None, ..) => DiffTarget::World(None),
            (Some(Include(commit)), None, _) => DiffTarget::World(Some(commit)),
            (Some(Between { from, to }), None, _) => DiffTarget::Commits { old: from, new: to },
            (Some(Include(old)), Some(Include(new)), None) => DiffTarget::Commits { old, new },
            _ => return Err(UsageError::DiffShape.into()),
        };
        Ok(DiffArgs {
            target,
            selection: self.selection,
            output: self.output,
            show_values: self.show_values,
        })
    }
}

parsed_through!(DiffArgs, RawDiffArgs);

/// World-portion selection shared by commit, status, checkout, and diff:
/// spelled inline, or named by a scope saved in the store.
#[derive(Debug, Clone)]
pub enum Selection {
    /// `--in`/`--region`/`--kind` spelled on the command line.
    Inline(InlineSelection),
    /// `--scope NAME`, resolved against the store at runtime.
    Scope(ScopeName),
}

impl Selection {
    /// Whether any selection option was given: `checkout` restores an
    /// area only then.
    pub const fn is_given(&self) -> bool {
        match self {
            Self::Inline(inline) => {
                !(inline.areas.is_empty() && inline.region.is_empty() && inline.kind.is_empty())
            }
            Self::Scope(_) => true,
        }
    }

    /// One-line human summary of the selected scope for pre-run echoes.
    pub fn describe(&self) -> String {
        match self {
            Self::Inline(inline) => inline.describe(),
            // The caller labels it `scope:` already.
            Self::Scope(name) => name.to_string(),
        }
    }
}

/// The selection flags as clap parses them. clap cannot derive `Args` for
/// an enum (clap-rs/clap#2621), so [`Selection`] delegates to this struct
/// and converts right after parsing.
#[derive(Debug, Clone, clap::Args)]
struct SelectionArgs {
    #[command(flatten)]
    inline: InlineSelection,
    /// Named scope saved with `scope create`. Exclusive with `--in`,
    /// `--region`, and `--kind`; not repeatable.
    #[arg(long, value_name = "NAME", conflicts_with_all = ["areas", "region", "kind"])]
    scope: Option<ScopeName>,
}

impl SelectionArgs {
    fn into_selection(self) -> Selection {
        self.scope
            .map_or(Selection::Inline(self.inline), Selection::Scope)
    }
}

impl clap::FromArgMatches for Selection {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        SelectionArgs::from_arg_matches(matches).map(SelectionArgs::into_selection)
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        // The variant may change, so a field-wise update does not apply.
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}

impl clap::Args for Selection {
    fn group_id() -> Option<clap::Id> {
        SelectionArgs::group_id()
    }

    fn augment_args(cmd: clap::Command) -> clap::Command {
        SelectionArgs::augment_args(cmd)
    }

    fn augment_args_for_update(cmd: clap::Command) -> clap::Command {
        SelectionArgs::augment_args_for_update(cmd)
    }
}

/// Selection spelled with `--in`, `--region`, and `--kind`.
///
/// Each `--in` picks one dimension whole, one chunk, or one rectangle;
/// `--region` is shorthand for a region-aligned rectangle. Entries compose
/// by union; unlisted dimensions are out. No `--in`/`--region` selects the
/// whole world. `--kind` applies to every selected area.
#[derive(Debug, Clone, clap::Args)]
pub struct InlineSelection {
    /// Dimension area: `DIM` (whole), `DIM:x,z` (chunk), or
    /// `DIM:x0,z0..x1,z1` (rectangle, inclusive). Repeatable.
    #[arg(
        long = "in",
        value_name = "DIM[:X,Z|X0,Z0..X1,Z1]",
        allow_hyphen_values = true
    )]
    pub areas: Vec<AreaSpec>,
    /// Region `DIM:RX,RZ`: every chunk of the file (rectangle sugar).
    /// Repeatable, additive with `--in`.
    #[arg(long, value_name = "DIM:RX,RZ", allow_hyphen_values = true)]
    pub region: Vec<RegionSpec>,
    /// Region families applied to every selected area. Empty means all.
    #[arg(long)]
    pub kind: Vec<RegionKind>,
}

impl InlineSelection {
    /// One-line human summary of the selected areas.
    pub fn describe(&self) -> String {
        if self.areas.is_empty() && self.region.is_empty() {
            return if self.kind.is_empty() {
                "whole world".to_owned()
            } else {
                format!("whole world, kinds {}", self.kind_summary())
            };
        }
        let mut parts = Vec::new();
        if !self.areas.is_empty() {
            parts.push(format!("{} area(s)", self.areas.len()));
        }
        if !self.region.is_empty() {
            parts.push(format!("{} region(s)", self.region.len()));
        }
        if !self.kind.is_empty() {
            parts.push(format!("kinds {}", self.kind_summary()));
        }
        parts.join(", ")
    }

    /// Comma-separated explicit `--kind` values.
    fn kind_summary(&self) -> String {
        self.kind
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Region kinds covered: explicit set, or all when unlisted.
    pub fn kinds(&self) -> Vec<RegionKind> {
        if self.kind.is_empty() {
            Scope::all_kinds()
        } else {
            let mut kinds = self.kind.clone();
            kinds.sort();
            kinds.dedup();
            kinds
        }
    }

    /// Explicit chunk coordinates from `--in DIM:x,z`, expanded across
    /// `--kind` (grouped by dimension upstream of scope building).
    pub fn explicit_chunks(&self) -> Vec<ChunkCoord> {
        let kinds = self.kinds();
        self.areas
            .iter()
            .filter_map(|spec| match spec.area {
                DimArea::Chunk(xz) => Some((spec.dim, xz)),
                _ => None,
            })
            .flat_map(|(dim, xz)| {
                kinds
                    .iter()
                    .map(move |kind| ChunkCoord::new(dim, *kind, xz.x, xz.z))
            })
            .collect()
    }

    /// Whether the selection holds whole-dimension or rectangle areas
    /// (including `--region`), which need coordinate enumeration.
    pub fn has_broad_areas(&self) -> bool {
        self.areas
            .iter()
            .any(|spec| !matches!(spec.area, DimArea::Chunk(_)))
            || !self.region.is_empty()
    }

    /// Owned scope for these flags. Empty means the whole world.
    pub fn owned_scope(&self) -> Scope {
        if self.areas.is_empty() && self.region.is_empty() {
            // `--kind` alone still narrows: dropping it here would silently
            // widen the operation to every region family.
            return if self.kind.is_empty() {
                Scope::World
            } else {
                Scope::Kinds(self.kinds())
            };
        }
        let kinds = self.kinds();
        let mut chunks: std::collections::BTreeMap<Dimension, Vec<ChunkCoord>> =
            std::collections::BTreeMap::new();
        let mut areas: Vec<(Dimension, Area)> = Vec::new();
        for spec in &self.areas {
            match spec.area {
                DimArea::All => areas.push((spec.dim, Area::All)),
                DimArea::Rect(rect) => areas.push((spec.dim, Area::Rect(rect))),
                DimArea::Chunk(xz) => {
                    for kind in &kinds {
                        chunks
                            .entry(spec.dim)
                            .or_default()
                            .push(ChunkCoord::new(spec.dim, *kind, xz.x, xz.z));
                    }
                }
            }
        }
        for region in &self.region {
            let (dim, rect) = region.area();
            areas.push((dim, Area::Rect(rect)));
        }
        for (dim, coords) in chunks {
            areas.push((dim, Area::Chunks(coords)));
        }
        Scope::Select { kinds, areas }
    }
}

/// One `--in` area: whole dimension, single chunk, or rectangle.
#[derive(Debug, Clone, Copy)]
pub struct AreaSpec {
    /// Selected dimension.
    pub dim: Dimension,
    /// Whole, chunk, or rectangle within it.
    pub area: DimArea,
}

/// Area shape within one dimension.
#[derive(Debug, Clone, Copy)]
pub enum DimArea {
    /// Every chunk of the dimension.
    All,
    /// Single chunk.
    Chunk(Xz),
    /// Inclusive rectangle.
    Rect(Rect),
}

impl std::str::FromStr for AreaSpec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_as(s, "DIM[:X,Z|X0,Z0..X1,Z1]")
    }
}

impl AreaSpec {
    /// Parse `s`, naming `expected` in dimension errors so each caller
    /// advertises only the forms it accepts.
    fn parse_as(s: &str, expected: &str) -> Result<Self, String> {
        let (dim, rest) = match s.split_once(':') {
            Some((dim, rest)) => (
                dim.parse::<Dimension>().map_err(|_| {
                    format!("invalid dimension {dim:?} in {s:?}, expected {expected}")
                })?,
                rest,
            ),
            None => (
                s.parse::<Dimension>()
                    .map_err(|_| format!("invalid dimension {s:?}, expected {expected}"))?,
                "",
            ),
        };
        let area = if rest.is_empty() {
            DimArea::All
        } else if let Some((first, second)) = rest.split_once("..") {
            let from = parse_pair(first, s)?;
            let to = parse_pair(second, s)?;
            DimArea::Rect(Rect::new(from.x, from.z, to.x, to.z))
        } else {
            DimArea::Chunk(parse_pair(rest, s)?)
        };
        Ok(Self { dim, area })
    }
}

/// One `scope create --in` area: a chunk or a rectangle, never a whole
/// dimension, since a named scope stores only chunk rectangles (ADR-0021).
#[derive(Debug, Clone, Copy)]
pub struct ScopeAreaSpec {
    /// Selected dimension.
    pub dim: Dimension,
    /// Inclusive rectangle; a single chunk is its 1x1 rectangle.
    pub rect: Rect,
}

impl std::str::FromStr for ScopeAreaSpec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        const EXPECTED: &str = "DIM:X,Z or DIM:X0,Z0..X1,Z1";
        let spec = AreaSpec::parse_as(s, EXPECTED)?;
        let rect = match spec.area {
            DimArea::Chunk(xz) => Rect::new(xz.x, xz.z, xz.x, xz.z),
            DimArea::Rect(rect) => rect,
            DimArea::All => {
                return Err(format!(
                    "scope area {s:?} names a whole dimension, expected {EXPECTED}"
                ));
            }
        };
        Ok(Self {
            dim: spec.dim,
            rect,
        })
    }
}

/// One `--region DIM:RX,RZ` entry.
#[derive(Debug, Clone, Copy)]
pub struct RegionSpec {
    /// Selected dimension.
    pub dim: Dimension,
    /// Region coordinates.
    pub at: Xz,
}

impl RegionSpec {
    /// The region file as a chunk rectangle.
    pub const fn area(&self) -> (Dimension, Rect) {
        (self.dim, Rect::region(self.at.x, self.at.z))
    }
}

impl std::str::FromStr for RegionSpec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (dim, rest) = s
            .split_once(':')
            .ok_or_else(|| format!("expected DIM:RX,RZ, got {s:?}"))?;
        Ok(Self {
            dim: dim
                .parse::<Dimension>()
                .map_err(|_| format!("invalid dimension {dim:?} in {s:?}, expected DIM:RX,RZ"))?,
            at: parse_pair(rest, s)?,
        })
    }
}

fn parse_pair(s: &str, whole: &str) -> Result<Xz, String> {
    let (x, z) = s
        .split_once(',')
        .ok_or_else(|| format!("expected X,Z in {whole:?}"))?;
    Ok(Xz {
        x: x.trim()
            .parse()
            .map_err(|_| format!("invalid X coordinate {x:?} in {whole:?}, expected X,Z"))?,
        z: z.trim()
            .parse()
            .map_err(|_| format!("invalid Z coordinate {z:?} in {whole:?}, expected X,Z"))?,
    })
}

/// `X,Z` coordinate pair inside `--in` and `--region` specs.
#[derive(Debug, Clone, Copy)]
pub struct Xz {
    pub x: i32,
    pub z: i32,
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    #[test]
    fn usage_errors_are_worded_in_one_place() {
        let cases = [
            (
                UsageError::NameRequired(Named::Branch),
                "branch name required",
            ),
            (UsageError::NameRequired(Named::Tag), "tag name required"),
            (
                UsageError::TooManyArguments(Extra::Any),
                "too many arguments",
            ),
            (
                UsageError::TooManyArguments(Extra::Rename),
                "too many arguments for a rename operation",
            ),
            (
                UsageError::TooManyArguments(Extra::ShowCurrent),
                "--show-current takes no arguments",
            ),
            (
                UsageError::DiffShape,
                "diff takes at most two commit-ishes, or one range <a>..<b>",
            ),
        ];
        for (err, message) in cases {
            assert_eq!(err.to_string(), message);
        }
        let err = ref_name("a..b").unwrap_err().to_string();
        assert!(err.starts_with("invalid ref name 'a..b': "), "{err}");
        let err = revision("a~x").unwrap_err().to_string();
        assert!(err.starts_with("invalid revision 'a~x': "), "{err}");
    }
}
