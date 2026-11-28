//! Command-line argument parsing and selection structures.

use crate::style::ColorChoice;
use clap::{Parser, Subcommand};
use sekai_app::{Area, ChunkCoord, Dimension, Rect, RegionKind, Revision, Scope, ScopeName};
use sekai_storage_local::config::ConfigKey;
use std::path::{Path, PathBuf};

/// Chunk-level deduplicated version control for Minecraft region files.
#[derive(Debug, Parser)]
#[command(name = "sekai", version, about)]
pub struct Cli {
    /// Store directory (`init` creates it; every other command fails when
    /// it is missing). Defaults to `.sekai` when omitted.
    #[arg(long, global = true, default_value = ".sekai")]
    pub store: String,

    /// Colorize human output. JSON output is never colorized.
    #[arg(long, global = true, value_enum, default_value_t = ColorChoice::Auto)]
    pub color: ColorChoice,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a new store, or re-initialize an existing one. Every other
    /// command requires a store created this way.
    Init {
        /// Emit output as JSON instead of human text. See docs/json.md.
        #[arg(long)]
        json: bool,
    },
    /// Record the current world state as a new commit.
    Commit {
        /// World directory (the one containing `region/`, `DIM-1/`, or `dimensions/`).
        /// For Bukkit-family servers, pass the server root instead.
        world: PathBuf,
        /// Human or JSON rendering plus optional phase timings.
        #[command(flatten)]
        output: TimingArgs,
        /// Ingest worker count. `0` means one per CPU.
        #[arg(long, default_value = "0")]
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
        /// World directory to preview.
        world: PathBuf,
        /// Human or JSON rendering plus optional phase timings.
        #[command(flatten)]
        output: TimingArgs,
        /// World portion to preview (default: whole world).
        #[command(flatten)]
        selection: Selection,
        /// Preview worker count. `0` means one per CPU.
        #[arg(long, default_value = "0")]
        jobs: usize,
    },
    /// Rebuild the world from a commit, overwriting region files.
    Checkout {
        /// World directory to rebuild in place.
        world: PathBuf,
        /// Revision to restore: a commit ID, a branch, or `HEAD` / `@`,
        /// optionally followed by one `~<n>` (see `log`).
        commit: Revision,
        /// Human or JSON rendering plus optional phase timings.
        #[command(flatten)]
        output: TimingArgs,
        /// Rebuild worker count. `0` means one per CPU.
        #[arg(long, default_value = "0")]
        jobs: usize,
        /// World portion to rebuild (default: whole world). Files outside
        /// the selection are never written, deleted, or otherwise touched.
        #[command(flatten)]
        selection: Selection,
        /// Keep region files unknown to the commit instead of deleting
        /// them (files created after the commit).
        #[arg(long)]
        keep_post_commit_files: bool,
        /// Keep live bytes for chunks unknown to the commit (created
        /// afterwards inside a commit-known region) instead of dropping
        /// them.
        #[arg(long)]
        keep_post_commit_chunks: bool,
        /// Keep live bytes for commit-tombstoned chunks instead of
        /// removing them. Fully tombstoned region files are left alone
        /// rather than deleted.
        #[arg(long)]
        keep_tombstoned_chunks: bool,
        /// How to handle a commit blob missing from CAS.
        #[arg(long, value_enum, default_value_t = OnMissingBlob::Abort)]
        on_missing_blob: OnMissingBlob,
        /// Where to rebuild a commit-known region whose file is missing
        /// on disk.
        #[arg(long, value_enum, default_value_t = OnMissingFile::SiblingFirst)]
        on_missing_file: OnMissingFile,
    },
    /// List recorded commits, oldest first.
    Log {
        /// Emit commit list as JSON instead of human text.
        /// See docs/json.md.
        #[arg(long)]
        json: bool,
        /// Include per-commit change statistics.
        #[arg(long)]
        stat: bool,
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
    /// Compare chunk NBT AST between two commits or between world state and a commit.
    Diff(DiffArgs),
    /// Restore selected world portions from a source commit (git restore):
    /// overwrites only the selection, records nothing, and moves no ref.
    Restore(RestoreArgs),
    /// List branches.
    Branch(BranchArgs),
    /// Manage worktrees: the worlds a store backs up, each with its own HEAD.
    Worktree {
        #[command(subcommand)]
        action: WorktreeCommand,
    },
    /// Read and write store configuration (`<store>/config.toml`), like
    /// `git config` limited to the repository scope.
    Config(ConfigArgs),
}

impl Command {
    /// Stable command name for JSON envelopes and error objects.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Init { .. } => "init",
            Self::Commit { .. } => "commit",
            Self::Status { .. } => "status",
            Self::Checkout { .. } => "checkout",
            Self::Log { .. } => "log",
            Self::Scope { .. } => "scope",
            Self::Cache { .. } => "cache",
            Self::Diff(_) => "diff",
            Self::Restore(_) => "restore",
            Self::Branch(_) => "branch",
            Self::Worktree { .. } => "worktree",
            Self::Config(_) => "config",
        }
    }

    /// Whether the command runs in JSON mode (`--json`). Errors serialize as
    /// an envelope on stdout instead of styled text on stderr.
    pub const fn output_json(&self) -> bool {
        match self {
            Self::Commit { output, .. }
            | Self::Status { output, .. }
            | Self::Checkout { output, .. } => output.json,
            Self::Init { json } | Self::Log { json, .. } => *json,
            Self::Scope { action } => action.output_json(),
            Self::Cache { action } => action.output_json(),
            Self::Diff(args) => args.output.json,
            Self::Restore(args) => args.output.json,
            Self::Branch(args) => args.json,
            Self::Worktree { action } => action.output_json(),
            Self::Config(args) => args.json,
        }
    }
}

/// `branch`: list every branch. Filtering is left to the shell (`grep`,
/// or `jq` over `--json`), so Git's `--list <pattern>` is not adopted.
#[derive(Debug, clap::Args)]
pub struct BranchArgs {
    /// Emit output as JSON instead of human text. See docs/json.md.
    #[arg(long)]
    pub json: bool,
}

/// Worktree operations (ADR-0038).
///
/// A worktree is one world the store backs up, opened from a store
/// location: the store root itself (the main worktree) or a store link
/// file (a linked one). Worktrees are a local-backend
/// feature; the backend-neutral layers know nothing of them.
#[derive(Debug, Subcommand)]
pub enum WorktreeCommand {
    /// List every worktree: the store's own (main) one first, then the
    /// linked ones in path order.
    List(WorktreeListArgs),
    /// Remove a linked worktree: delete its store link and registration,
    /// leaving the world directory it backs up untouched.
    Remove(WorktreeRemoveArgs),
}

impl WorktreeCommand {
    /// Whether the operation runs in JSON mode (`--json`).
    const fn output_json(&self) -> bool {
        match self {
            Self::List(args) => args.json,
            Self::Remove(args) => args.json,
        }
    }
}

/// `worktree list` arguments, mirroring `git worktree list [-v | --porcelain [-z]]`.
/// Git's `--expire` is not adopted: it annotates prunable worktrees, and
/// sekai has no prune.
// The flags are Git's independent switches plus sekai's `--json`; each
// maps one-to-one to a CLI argument, so bools are the honest shape.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, clap::Args)]
pub struct WorktreeListArgs {
    /// Machine-readable output: one record block per worktree
    /// (`worktree`, `HEAD`, then `branch` or `detached`), blank-line
    /// separated.
    #[arg(long, conflicts_with = "verbose")]
    pub porcelain: bool,
    /// Terminate porcelain records with a NUL character instead of a
    /// newline. Requires `--porcelain`, as in Git.
    #[arg(short = 'z', requires = "porcelain")]
    pub z: bool,
    /// Accepted for compatibility; sekai worktrees carry no locked or
    /// prunable annotations, so this flag has no effect.
    #[arg(short = 'v', long)]
    pub verbose: bool,
    /// Emit output as JSON instead of human text. See docs/json.md.
    #[arg(long, conflicts_with_all = ["porcelain", "z"])]
    pub json: bool,
}

/// `config`: git-style settings in `<store>/config.toml`.
///
/// Only the store scope exists, so git's `--global`/`--system`/`--local`
/// location flags do not apply. Every key holds one typed value, so the
/// multivar actions (`--add`, `--replace-all`, `--get-all`, `--unset-all`)
/// and the regex/section operations have no meaning here.
#[derive(Debug, clap::Args)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "mutually exclusive action flags; clap enforces the exclusivity"
)]
pub struct ConfigArgs {
    /// Config key, `section.name` (e.g. `user.name`), case-insensitive.
    /// With no value and no action flag, reads the key.
    #[arg(value_name = "KEY", required_unless_present = "list")]
    pub key: Option<ConfigKey>,
    /// New value for the key; an empty value is refused (use `--unset`).
    #[arg(
        value_name = "VALUE",
        value_parser = non_empty_value,
        conflicts_with_all = ["get", "unset", "list", "default"],
    )]
    pub value: Option<String>,
    /// Read the key's value (the default action for a bare key).
    #[arg(long, conflicts_with_all = ["unset", "list"], requires = "key")]
    pub get: bool,
    /// Remove the key from the config file.
    #[arg(long, conflicts_with_all = ["list", "default"], requires = "key")]
    pub unset: bool,
    /// List every set key as `key=value`.
    #[arg(long, short = 'l', conflicts_with_all = ["key", "default"])]
    pub list: bool,
    /// With `--list`, print key names only. Human output only, like
    /// `--progress`: JSON always carries the full entries.
    #[arg(
        long,
        requires = "list",
        conflicts_with_all = ["key", "value", "get", "unset", "default", "json"],
    )]
    pub name_only: bool,
    /// Value to report when a read key is unset (exit 0 instead of 1).
    #[arg(long, value_name = "VALUE", conflicts_with_all = ["unset", "list"])]
    pub default: Option<String>,
    /// Emit output as JSON instead of human text. See docs/json.md.
    #[arg(long)]
    pub json: bool,
}

/// A stored config value is never empty: empty reads as unset, so an
/// empty write would silently remove the key instead of setting it.
fn non_empty_value(s: &str) -> Result<String, String> {
    if s.is_empty() {
        Err("config value must not be empty; use --unset to remove a key".to_owned())
    } else {
        Ok(s.to_owned())
    }
}

/// `worktree remove` arguments, mirroring `git worktree remove [-f] <worktree>`.
/// The positional is the store location the worktree opens (its store link
/// file), in any spelling `--store` accepts for it.
#[derive(Debug, clap::Args)]
pub struct WorktreeRemoveArgs {
    /// Store location of the worktree to remove: the store link file a
    /// linked worktree opens. The store's own (main) worktree cannot be
    /// removed.
    pub worktree: PathBuf,
    /// Accepted for compatibility; sekai worktrees carry no dirty or locked
    /// state, so this flag has no effect. The main-worktree and unregistered
    /// refusals are never overridden, as in Git. Repeatable, as Git's force
    /// is (`-f -f`).
    #[arg(short = 'f', long, overrides_with = "force")]
    pub force: bool,
    /// Emit output as JSON instead of human text. See docs/json.md.
    #[arg(long)]
    pub json: bool,
}

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
    const fn output_json(&self) -> bool {
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
        /// World directory to hash (the one containing `region/`, `DIM-1/`,
        /// or `dimensions/`). For Bukkit-family servers, pass the server
        /// root instead. Defaults to the current directory.
        #[arg(long, value_name = "PATH", default_value = ".")]
        world: PathBuf,
        /// Human or JSON rendering plus optional phase timings.
        #[command(flatten)]
        output: TimingArgs,
        /// Hashing worker count. `0` means one per CPU.
        #[arg(long, default_value = "0")]
        jobs: usize,
    },
}

impl CacheCommand {
    /// Whether the operation runs in JSON mode (`--json`).
    const fn output_json(&self) -> bool {
        match self {
            Self::Clean { json } => *json,
            Self::Update { output, .. } => output.json,
        }
    }
}

/// `--timing`/`--json`/`--progress` flags shared by every report command
/// except `log` (which has no phases to time or items to count).
#[derive(Debug, Clone, Copy, clap::Args)]
pub struct TimingArgs {
    /// Print a per-phase timing breakdown after the report.
    #[arg(long)]
    pub timing: bool,
    /// Emit output as JSON instead of human text. Combined with
    /// `--timing`, phase timings are included. See docs/json.md.
    #[arg(long)]
    pub json: bool,
    /// Show a progress bar on stderr. Refused with `--json`, and
    /// silent without a stderr TTY.
    #[arg(long, conflicts_with = "json")]
    pub progress: bool,
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

#[derive(Debug, clap::Args)]
pub struct DiffArgs {
    /// World directory to compare against a commit (if specified).
    #[arg(long)]
    pub world: Option<PathBuf>,
    /// Older revision (a commit ID, a branch, or `HEAD`, with optional
    /// `~<n>`; when comparing commits and omitted, defaults to the lowest-ID
    /// parent of the newer side). With `--world`, the single commit to
    /// compare against (default: the active ref's target).
    pub old_commit: Option<Revision>,
    /// Newer revision (if omitted, defaults to the active ref's target).
    /// Commit-vs-commit only.
    #[arg(conflicts_with = "world")]
    pub new_commit: Option<Revision>,
    /// Chunks to compare (default: whole world). Only chunks with
    /// differences are reported.
    #[command(flatten)]
    pub selection: Selection,
    /// Human or JSON rendering plus optional phase timings.
    #[command(flatten)]
    pub output: TimingArgs,
    /// Accepted for compatibility; the chunk diff carries normalized
    /// hashes, not per-tag values, so this flag has no effect.
    #[arg(long)]
    pub show_values: bool,
}

impl DiffArgs {
    /// What this invocation compares. Total over every argument shape clap
    /// accepts: `--world` compares against at most one commit (the second
    /// positional is refused at parse time), and the commit pair defaults
    /// resolve against the store at runtime.
    pub(crate) fn target(&self) -> DiffTarget<'_> {
        self.world.as_ref().map_or(
            DiffTarget::Commits {
                old: self.old_commit.as_ref(),
                new: self.new_commit.as_ref(),
            },
            |world| DiffTarget::World {
                world,
                commit: self.old_commit.as_ref(),
            },
        )
    }
}

/// Comparison sides of a `diff` run, with the `--world`-versus-positional
/// ambiguity resolved statically instead of juggling `Option`s at runtime.
#[derive(Debug)]
pub enum DiffTarget<'a> {
    /// Live world state against one commit (`None` means the active
    /// ref's target).
    World {
        world: &'a Path,
        commit: Option<&'a Revision>,
    },
    /// Commit against commit (`None` newer side means the active ref's
    /// target; `None` older side means the newer side's lowest-ID parent).
    /// A root commit has no implicit older side and errors; pass it or
    /// `--world` explicitly in that case.
    Commits {
        old: Option<&'a Revision>,
        new: Option<&'a Revision>,
    },
}

/// `restore` arguments: git restore's shape over sekai's selection flags.
///
/// Sekai has no index, so git's `--staged`/`--worktree` locations do not
/// apply: the live world is always the restore location, and the default
/// source is the active ref's target (`HEAD`) instead of the index.
#[derive(Debug, clap::Args)]
pub struct RestoreArgs {
    /// World directory to restore in place.
    pub(crate) world: PathBuf,
    /// Restore source: a commit ID, a branch, or `HEAD` / `@`, optionally
    /// followed by one `~<n>` (see `log`). Defaults to the active ref's
    /// target (`HEAD`).
    #[arg(long, short = 's', value_name = "REV")]
    pub source: Option<Revision>,
    /// Overlay mode: keep region files unknown to the source instead of
    /// deleting them. The default (no-overlay) deletes selected files the
    /// source does not know, matching git restore.
    #[arg(long)]
    pub overlay: bool,
    /// Human or JSON rendering plus optional phase timings.
    #[command(flatten)]
    pub(crate) output: TimingArgs,
    /// Rebuild worker count. `0` means one per CPU.
    #[arg(long, default_value = "0")]
    pub jobs: usize,
    /// World portion to restore. Required, like git restore's pathspec:
    /// spell `--in`/`--region` areas or name a saved `--scope`.
    #[command(flatten)]
    pub(crate) selection: Selection,
    /// How to handle a source blob missing from CAS.
    #[arg(long, value_enum, default_value_t = OnMissingBlob::Abort)]
    pub(crate) on_missing_blob: OnMissingBlob,
    /// Where to rebuild a source-known region whose file is missing on
    /// disk.
    #[arg(long, value_enum, default_value_t = OnMissingFile::SiblingFirst)]
    pub(crate) on_missing_file: OnMissingFile,
}

impl RestoreArgs {
    /// Whether the selection spells at least one path. git restore
    /// demands a pathspec; an empty selection would silently widen the
    /// run to the whole world, which is checkout's job. `--kind` alone
    /// narrows families but spells no path, so it does not count.
    pub(crate) const fn has_paths(&self) -> bool {
        match &self.selection {
            Selection::Inline(inline) => !inline.areas.is_empty() || !inline.region.is_empty(),
            Selection::Scope(_) => true,
        }
    }
}

/// World-portion selection shared by commit, status, checkout, diff, and
/// restore: spelled inline, or named by a scope saved in the store.
#[derive(Debug, Clone)]
pub enum Selection {
    /// `--in`/`--region`/`--kind` spelled on the command line.
    Inline(InlineSelection),
    /// `--scope NAME`, resolved against the store at runtime.
    Scope(ScopeName),
}

impl Selection {
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
    fn describe(&self) -> String {
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
    at: Xz,
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
    x: i32,
    z: i32,
}
