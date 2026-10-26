//! Command-line argument parsing and selection structures.

use crate::style::ColorChoice;
use clap::{Parser, Subcommand};
use sekai_app::{Area, ChunkCoord, CommitId, Dimension, Rect, RefName, RegionKind, Scope};
use std::path::{Path, PathBuf};

/// Chunk-level deduplicated version control for Minecraft region files.
#[derive(Debug, Parser)]
#[command(name = "sekai", version, about)]
pub struct Cli {
    /// Store directory (`commit` creates it when missing; other commands
    /// fail). Defaults to `.sekai` when omitted. Prefix with `sqlite://`
    /// explicitly if preferred.
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
    /// Record the current world state as a new commit.
    #[command(visible_alias = "backup")]
    Commit {
        /// World directory (the one containing `region/`, `DIM-1/`, or `dimensions/`).
        /// For Bukkit-family servers, pass the server root instead.
        world: PathBuf,
        /// Human or JSON rendering plus optional phase timings.
        #[command(flatten)]
        output: TimingArgs,
        /// Derive volatile diff views alongside blobs.
        #[arg(long)]
        with_diff: bool,
        /// Ingest worker count. `0` means one per CPU.
        #[arg(long, default_value = "0")]
        jobs: usize,
        /// Show a progress bar on stderr. Refused with `--json`, and
        /// silent without a stderr TTY.
        #[arg(long, conflicts_with = "json")]
        progress: bool,
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
        /// Show a progress bar on stderr. Refused with `--json`, and
        /// silent without a stderr TTY.
        #[arg(long, conflicts_with = "json")]
        progress: bool,
        /// World portion to preview (default: whole world).
        #[command(flatten)]
        selection: Selection,
        /// Preview worker count. `0` means one per CPU.
        #[arg(long, default_value = "0")]
        jobs: usize,
    },
    /// Rebuild the world from a commit, overwriting region files.
    #[command(visible_alias = "rollback")]
    Checkout {
        /// World directory to rebuild in place.
        world: PathBuf,
        /// Commit reference to restore (`<64-char hash>` or a ref name, see `log`).
        commit: CommitRef,
        /// Human or JSON rendering plus optional phase timings.
        #[command(flatten)]
        output: TimingArgs,
        /// Show a progress bar on stderr. Refused with `--json`, and
        /// silent without a stderr TTY.
        #[arg(long, conflicts_with = "json")]
        progress: bool,
        /// World portion to rebuild (default: whole world). Files outside
        /// the selection are never written, deleted, or otherwise touched.
        #[command(flatten)]
        selection: Selection,
        /// Keep region files unknown to the commit instead of deleting
        /// them (files created after the commit).
        #[arg(long, alias = "keep-post-snapshot-files")]
        keep_post_commit_files: bool,
        /// Keep live bytes for chunks unknown to the commit (created
        /// afterwards inside a commit-known region) instead of dropping
        /// them.
        #[arg(long, alias = "keep-post-snapshot-chunks")]
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
    #[command(visible_alias = "list")]
    Log {
        /// Emit commit list as JSON instead of human text.
        /// See docs/json.md.
        #[arg(long)]
        json: bool,
        /// Include per-commit change statistics.
        #[arg(long)]
        stat: bool,
    },
    /// Rebuild a commit into a fresh directory (never touches the live world).
    #[command(visible_alias = "export")]
    Clone {
        /// Commit reference to clone (`<64-char hash>` or a ref name, see `log`).
        commit: CommitRef,
        /// Directory to rebuild the commit into (created when missing;
        /// must otherwise be empty).
        out: PathBuf,
        /// Directory layout for the rebuilt world.
        #[arg(long, value_enum, default_value_t = CloneFlavor::Legacy)]
        flavor: CloneFlavor,
        /// Overworld folder name for `--flavor bukkit` (the `level-name`).
        #[arg(long, default_value = "world")]
        base: String,
        /// How to handle a commit blob missing from CAS.
        #[arg(long, value_enum, default_value_t = OnMissingBlob::Abort)]
        on_missing_blob: OnMissingBlob,
        /// World portion to clone (default: whole world).
        #[command(flatten)]
        selection: Selection,
        /// Human or JSON rendering plus optional phase timings.
        #[command(flatten)]
        output: TimingArgs,
        /// Show a progress bar on stderr. Refused with `--json`, and
        /// silent without a stderr TTY.
        #[arg(long, conflicts_with = "json")]
        progress: bool,
    },
    /// Manage refs: named, movable pointers to commits.
    Ref {
        #[command(subcommand)]
        action: RefCommand,
    },
    /// Compare chunk NBT AST between two commits or between world state and a commit.
    Diff(DiffArgs),
    /// Delete old commits, folding their rows into retained ones.
    /// At least one of `--keep-last` / `--before` is required.
    Prune {
        /// Keep the newest N commits.
        #[arg(long, required_unless_present = "before")]
        keep_last: Option<u64>,
        /// Retain this commit and everything newer (`<64-char hash>` or a ref name).
        #[arg(long, required_unless_present = "keep_last")]
        before: Option<CommitRef>,
        /// Show what would be deleted without deleting anything.
        #[arg(long)]
        dry_run: bool,
        /// Human or JSON rendering plus optional phase timings.
        #[command(flatten)]
        output: TimingArgs,
        /// Show a progress bar on stderr. Refused with `--json`, and
        /// silent without a stderr TTY. Refused with `--dry-run`, which
        /// has no apply phase to report progress for.
        #[arg(long, conflicts_with_all = ["json", "dry_run"])]
        progress: bool,
    },
    /// Garbage collect unreferenced orphan blobs from the store.
    Gc {
        /// Inspect store and build plan without unlinking orphan blobs.
        #[arg(long)]
        dry_run: bool,
        /// Human or JSON rendering plus optional phase timings.
        #[command(flatten)]
        output: TimingArgs,
        /// Show a progress bar on stderr. Refused with `--json`, and
        /// silent without a stderr TTY. Refused with `--dry-run`, which
        /// has no apply phase to report progress for.
        #[arg(long, conflicts_with_all = ["json", "dry_run"])]
        progress: bool,
    },
}

impl Command {
    /// Stable command name for JSON envelopes and error objects.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Commit { .. } => "commit",
            Self::Status { .. } => "status",
            Self::Checkout { .. } => "checkout",
            Self::Log { .. } => "log",
            Self::Clone { .. } => "clone",
            Self::Ref { .. } => "ref",
            Self::Prune { .. } => "prune",
            Self::Diff(_) => "diff",
            Self::Gc { .. } => "gc",
        }
    }

    /// Whether the command runs in JSON mode (`--json`). Errors serialize as
    /// an envelope on stdout instead of styled text on stderr.
    pub const fn output_json(&self) -> bool {
        match self {
            Self::Commit { output, .. }
            | Self::Status { output, .. }
            | Self::Checkout { output, .. }
            | Self::Clone { output, .. }
            | Self::Prune { output, .. }
            | Self::Gc { output, .. } => output.json,
            Self::Log { json, .. } => *json,
            Self::Ref { action } => action.output_json(),
            Self::Diff(args) => args.output.json,
        }
    }
}

/// Ref operations. The name position is required and the commit target
/// is optional: a missing name and a `--force` outside `create` stay
/// statically unrepresentable.
#[derive(Debug, Subcommand)]
pub enum RefCommand {
    /// Point a ref at a commit.
    Create {
        /// Ref name (ASCII alphanumerics plus ".", "_", "-", "/"; 1-64
        /// bytes; not all digits). Written bare (never `@`-prefixed); ref
        /// names feed back as `RefName` tokens, not as `CommitRef`s.
        name: RefName,
        /// Commit reference (`<64-char hash>` or a ref name) to point at.
        /// Defaults to the active ref's target.
        commit: Option<CommitRef>,
        /// Move an existing ref instead of failing.
        #[arg(long)]
        force: bool,
        /// Emit output as JSON instead of human text. See docs/json.md.
        #[arg(long)]
        json: bool,
    },
    /// Delete a ref.
    Delete {
        /// Ref name to delete.
        name: RefName,
        /// Emit output as JSON instead of human text. See docs/json.md.
        #[arg(long)]
        json: bool,
    },
    /// List all refs in name order.
    List {
        /// Emit output as JSON instead of human text. See docs/json.md.
        #[arg(long)]
        json: bool,
    },
    /// Make a ref the active one: new commits hang from it.
    Activate {
        /// Ref name to activate.
        name: RefName,
        /// Emit output as JSON instead of human text. See docs/json.md.
        #[arg(long)]
        json: bool,
    },
}

impl RefCommand {
    /// Whether the operation runs in JSON mode (`--json`).
    pub const fn output_json(&self) -> bool {
        match self {
            Self::Create { json, .. }
            | Self::Delete { json, .. }
            | Self::List { json }
            | Self::Activate { json, .. } => *json,
        }
    }
}

/// Commit reference on the command line: `<64-char hash>` or a ref name.
///
/// Either a 64-character hexadecimal commit ID or a bare valid [`RefName`],
/// tried in that exclusive order: a bare token is parsed as a commit ID
/// first and only treated as a [`RefName`] when that fails. Because
/// [`RefName`] rejects all-digit names, the numeric ID namespace and the
/// ref-name namespace cannot overlap: a numeric token is always a commit ID
/// (or `UnknownCommit`), never a ref.
///
/// The shape is validated at parse time so a malformed reference never
/// reaches the store; resolving it to an existing commit stays a
/// runtime step. The grammar mirrors `core`'s `resolve_commit_ref`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRef(String);

impl CommitRef {
    /// Raw reference text (`<64-char hash>` or a ref name).
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CommitRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for CommitRef {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<CommitId>()
            .map(|id| {
                // Canonicalise so the stored spelling can never diverge from
                // the validation that accepted it.
                Self(id.to_string())
            })
            .or_else(|id_err| match RefName::parse(s) {
                Ok(_) => Ok(Self(s.to_owned())),
                Err(name_err) => Err(format!(
                    "invalid reference {s:?}: {name_err} (not a 64-char commit hash: {id_err}), expected <64-char commit hash> or <ref>"
                )),
            })
    }
}

/// `--timing`/`--json` pair shared by every report command except `log`
/// (which has no phases to time).
#[derive(Debug, Clone, Copy, clap::Args)]
pub struct TimingArgs {
    /// Print a per-phase timing breakdown after the report.
    #[arg(long)]
    pub timing: bool,
    /// Emit output as JSON instead of human text. Combined with
    /// `--timing`, phase timings are included. See docs/json.md.
    #[arg(long)]
    pub json: bool,
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

/// Directory layout for `clone` output.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum CloneFlavor {
    /// Single world folder (`region/`, `DIM-1/`, `DIM1/`).
    Legacy,
    /// Modern layout (`dimensions/minecraft/<name>/`, since 26.1).
    New,
    /// Bukkit-family split folders (`<base>/`, `<base>_nether/DIM-1/`,
    /// `<base>_the_end/DIM1/`).
    Bukkit,
}

#[derive(Debug, clap::Args)]
pub struct DiffArgs {
    /// World directory to compare against a commit (if specified).
    #[arg(long)]
    pub world: Option<PathBuf>,
    /// Older commit reference (`<64-char hash>` or `<ref>`; when comparing
    /// commits and omitted, defaults to the lowest-ID parent of the newer
    /// side). With `--world`, the single commit to compare against
    /// (default: the active ref's target).
    pub old_commit: Option<CommitRef>,
    /// Newer commit reference (`<64-char hash>` or `<ref>`; if omitted,
    /// defaults to the active ref's target). Commit-vs-commit only.
    #[arg(conflicts_with = "world")]
    pub new_commit: Option<CommitRef>,
    /// Chunks to compare (default: whole world). One chunk keeps the
    /// legacy single-chunk output; several switch to grouped output.
    #[command(flatten)]
    pub selection: Selection,
    /// Human or JSON rendering plus optional phase timings.
    #[command(flatten)]
    pub output: TimingArgs,
    /// Show a progress bar on stderr. Refused with `--json`, and
    /// silent without a stderr TTY.
    #[arg(long, conflicts_with = "json")]
    pub progress: bool,
    /// Show concrete old/new values in human output (SNBT format).
    #[arg(long)]
    pub show_values: bool,
}

impl DiffArgs {
    /// What this invocation compares. Total over every argument shape clap
    /// accepts: `--world` compares against at most one commit (the second
    /// positional is refused at parse time), and the commit pair defaults
    /// resolve against the store at runtime.
    pub fn target(&self) -> DiffTarget<'_> {
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
        commit: Option<&'a CommitRef>,
    },
    /// Commit against commit (`None` newer side means the active ref's
    /// target; `None` older side means the newer side's lowest-ID parent).
    /// A root commit has no implicit older side and errors; pass it or
    /// `--world` explicitly in that case.
    Commits {
        old: Option<&'a CommitRef>,
        new: Option<&'a CommitRef>,
    },
}

/// World-portion selection shared by commit, status, checkout, diff, and
/// clone.
///
/// Each `--in` picks one dimension whole, one chunk, or one rectangle;
/// `--region` is shorthand for a region-aligned rectangle. Entries compose
/// by union; unlisted dimensions are out. No `--in`/`--region` selects the
/// whole world. `--kind` applies to every selected area.
#[derive(Debug, Clone, clap::Args)]
pub struct Selection {
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

impl Selection {
    /// One-line human summary of the selected scope for pre-run echoes.
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

    /// Owned scope for this selection. Empty means the whole world.
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
            areas.push((
                region.dim,
                Area::Rect(Rect::region(region.at.x, region.at.z)),
            ));
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
        let (dim, rest) = match s.split_once(':') {
            Some((dim, rest)) => (
                dim.parse::<Dimension>().map_err(|_| {
                    format!("invalid dimension {dim:?} in {s:?}, expected DIM[:X,Z|X0,Z0..X1,Z1]")
                })?,
                rest,
            ),
            None => (
                s.parse::<Dimension>().map_err(|_| {
                    format!("invalid dimension {s:?}, expected DIM[:X,Z|X0,Z0..X1,Z1]")
                })?,
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

/// One `--region DIM:RX,RZ` entry.
#[derive(Debug, Clone, Copy)]
pub struct RegionSpec {
    /// Selected dimension.
    pub dim: Dimension,
    /// Region coordinates.
    pub at: Xz,
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
