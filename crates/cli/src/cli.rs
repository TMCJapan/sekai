//! Command-line argument parsing and selection structures.

use crate::style::ColorChoice;
use clap::{Parser, Subcommand};
use sekai_app::{Area, ChunkCoord, Dimension, Rect, RegionKind, Scope, TagName};
use std::path::PathBuf;

/// Chunk-level deduplicated snapshots for Minecraft region files.
#[derive(Debug, Parser)]
#[command(name = "sekai", version, about)]
pub struct Cli {
    /// Backup store directory (created when missing). Prefix with
    /// `sqlite://` explicitly if preferred.
    #[arg(long, global = true, default_value = "sekai-store")]
    pub store: String,

    /// Colorize human output. JSON output is never colorized.
    #[arg(long, global = true, value_enum, default_value_t = ColorChoice::Auto)]
    pub color: ColorChoice,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Record the current world state as a new snapshot.
    Backup {
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
    /// Preview what a backup would record, without writing anything.
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
    /// Rebuild the world from a snapshot, overwriting region files.
    Rollback {
        /// World directory to rebuild in place.
        world: PathBuf,
        /// Snapshot reference to restore (`<id>` or `@tag`, see `list`).
        snapshot: String,
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
        /// Keep region files unknown to the snapshot instead of deleting
        /// them (files created after the snapshot).
        #[arg(long)]
        keep_post_snapshot_files: bool,
        /// Keep live bytes for chunks unknown to the snapshot (created
        /// afterwards inside a snapshot-known region) instead of dropping
        /// them.
        #[arg(long)]
        keep_post_snapshot_chunks: bool,
        /// Keep live bytes for snapshot-tombstoned chunks instead of
        /// removing them. Fully tombstoned region files are left alone
        /// rather than deleted.
        #[arg(long)]
        keep_tombstoned_chunks: bool,
        /// How to handle a snapshot blob missing from CAS.
        #[arg(long, value_enum, default_value_t = OnMissingBlob::Abort)]
        on_missing_blob: OnMissingBlob,
        /// Where to rebuild a snapshot-known region whose file is missing
        /// on disk.
        #[arg(long, value_enum, default_value_t = OnMissingFile::SiblingFirst)]
        on_missing_file: OnMissingFile,
    },
    /// List recorded snapshots, oldest first.
    List {
        /// Emit snapshot list as JSON instead of human text.
        /// See docs/json.md.
        #[arg(long)]
        json: bool,
        /// Include per-snapshot change statistics.
        #[arg(long)]
        stat: bool,
    },
    /// Rebuild a snapshot into a fresh directory (never touches the live world).
    Export {
        /// Snapshot reference to export (`<id>` or `@tag`, see `list`).
        snapshot: String,
        /// Directory to rebuild the snapshot into (created when missing;
        /// must otherwise be empty).
        out: PathBuf,
        /// Directory layout for the rebuilt world.
        #[arg(long, value_enum, default_value_t = ExportFlavor::Legacy)]
        flavor: ExportFlavor,
        /// Overworld folder name for `--flavor bukkit` (the `level-name`).
        #[arg(long, default_value = "world")]
        base: String,
        /// How to handle a snapshot blob missing from CAS.
        #[arg(long, value_enum, default_value_t = OnMissingBlob::Abort)]
        on_missing_blob: OnMissingBlob,
        /// World portion to export (default: whole world).
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
    /// Tag snapshots with human-readable names.
    Tag {
        /// Tag name (`[A-Za-z0-9._-]`, 1-64 bytes, not all digits).
        /// Omitted with nothing else lists tags.
        name: Option<TagName>,
        /// Snapshot reference (`<id>` or `@tag`) to point at.
        snapshot: Option<String>,
        /// Delete the tag instead of creating it.
        #[arg(long, short = 'd', conflicts_with_all = ["snapshot", "force"])]
        delete: bool,
        /// Move an existing tag instead of failing.
        #[arg(long)]
        force: bool,
        /// Emit output as JSON instead of human text. See docs/json.md.
        #[arg(long)]
        json: bool,
    },
    /// Compare chunk NBT AST between two snapshots or between world state and a snapshot.
    Diff(DiffArgs),
    /// Delete old snapshots, folding their rows into retained ones.
    /// At least one of `--keep-last` / `--before` is required.
    Prune {
        /// Keep the newest N snapshots.
        #[arg(long, required_unless_present = "before")]
        keep_last: Option<u64>,
        /// Retain this snapshot and everything newer (`<id>` or `@tag`).
        #[arg(long, required_unless_present = "keep_last")]
        before: Option<String>,
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
    /// Read-only inspection helpers (never write to world or store).
    Debug {
        #[command(subcommand)]
        debug: DebugCommand,
    },
}

impl Command {
    /// Stable command name for JSON envelopes and error objects.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Backup { .. } => "backup",
            Self::Status { .. } => "status",
            Self::Rollback { .. } => "rollback",
            Self::List { .. } => "list",
            Self::Export { .. } => "export",
            Self::Tag { .. } => "tag",
            Self::Prune { .. } => "prune",
            Self::Diff(_) => "diff",
            Self::Gc { .. } => "gc",
            Self::Debug { debug } => match debug {
                DebugCommand::Scan { .. } => "scan",
            },
        }
    }

    /// Whether the command runs in JSON mode (`--json`). Errors serialize as
    /// an envelope on stdout instead of styled text on stderr.
    pub const fn output_json(&self) -> bool {
        match self {
            Self::Backup { output, .. }
            | Self::Status { output, .. }
            | Self::Rollback { output, .. }
            | Self::Export { output, .. }
            | Self::Prune { output, .. }
            | Self::Gc { output, .. } => output.json,
            Self::List { json, .. } | Self::Tag { json, .. } => *json,
            Self::Diff(args) => args.output.json,
            Self::Debug { debug } => match debug {
                DebugCommand::Scan { output, .. } => output.json,
            },
        }
    }
}

/// `--timing`/`--json` pair shared by every report command except `list`
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

/// What to do when a snapshot blob is absent from CAS.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum OnMissingBlob {
    /// Abort loudly (a missing blob is corruption).
    Abort,
    /// Skip the chunk, leaving it out of the rebuilt file.
    SkipChunk,
}

/// Where to rebuild a snapshot-known region whose file is missing on disk.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum OnMissingFile {
    /// Prefer a same-dimension sibling's directory, then the derived path.
    SiblingFirst,
    /// Always use the layout-derived path.
    DerivedOnly,
    /// Fail loudly instead of guessing a location.
    Error,
}

/// Directory layout for `export` output.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ExportFlavor {
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
    /// World directory to compare against snapshot (if specified).
    #[arg(long)]
    pub world: Option<PathBuf>,
    /// Older snapshot reference (`<id>` or `@tag`; if omitted when
    /// comparing snapshots, defaults to second-latest).
    pub old_snapshot: Option<String>,
    /// Newer snapshot reference (`<id>` or `@tag`; if omitted, defaults
    /// to latest snapshot).
    pub new_snapshot: Option<String>,
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

/// World-portion selection shared by backup, status, rollback, diff, and
/// export.
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
            return "whole world".to_owned();
        }
        let mut parts = Vec::new();
        if !self.areas.is_empty() {
            parts.push(format!("{} area(s)", self.areas.len()));
        }
        if !self.region.is_empty() {
            parts.push(format!("{} region(s)", self.region.len()));
        }
        if !self.kind.is_empty() {
            let kinds = self
                .kind
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",");
            parts.push(format!("kinds {kinds}"));
        }
        parts.join(", ")
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
            return Scope::World;
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

#[derive(Debug, Subcommand)]
pub enum DebugCommand {
    /// List region files with size, mtime, chunk count, and header hash.
    ///
    /// Read-only: never writes to the world or the store.
    Scan {
        /// World directory to inspect.
        world: PathBuf,
        /// Human or JSON rendering plus optional phase timings.
        #[command(flatten)]
        output: TimingArgs,
    },
}
