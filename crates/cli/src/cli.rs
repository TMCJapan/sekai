//! Command-line argument parsing and selection structures.

use crate::style::ColorChoice;
use clap::{Parser, Subcommand};
use sekai_app::{ChunkCoord, Dimension, RegionKey, RegionKind};
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
        /// World portion to record (default: whole world).
        #[command(flatten)]
        selection: Selection,
    },
    /// Rebuild the world from a snapshot, overwriting region files.
    Rollback {
        /// World directory to rebuild in place.
        world: PathBuf,
        /// Snapshot ID to restore (see `list`).
        snapshot: u64,
        /// Human or JSON rendering plus optional phase timings.
        #[command(flatten)]
        output: TimingArgs,
        /// World portion to rebuild (default: whole world). Files outside
        /// the selection are never written, deleted, or otherwise touched.
        #[command(flatten)]
        selection: Selection,
    },
    /// List recorded snapshots, oldest first.
    List {
        /// Emit snapshot list as JSON instead of human text.
        /// See docs/json.md.
        #[arg(long)]
        json: bool,
    },
    /// Compare chunk NBT AST between two snapshots or between world state and a snapshot.
    Diff(DiffArgs),
    /// Garbage collect unreferenced orphan blobs from the store.
    Gc {
        /// Inspect store and build plan without unlinking orphan blobs.
        #[arg(long)]
        dry_run: bool,
        /// Human or JSON rendering plus optional phase timings.
        #[command(flatten)]
        output: TimingArgs,
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
            Self::Rollback { .. } => "rollback",
            Self::List { .. } => "list",
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
            | Self::Rollback { output, .. }
            | Self::Gc { output, .. } => output.json,
            Self::List { json } => *json,
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

#[derive(Debug, clap::Args)]
pub struct DiffArgs {
    /// World directory to compare against snapshot (if specified).
    #[arg(long)]
    pub world: Option<PathBuf>,
    /// Older snapshot ID (if omitted when comparing snapshots, defaults to second-latest).
    pub old_snapshot: Option<u64>,
    /// Newer snapshot ID (if omitted, defaults to latest snapshot).
    pub new_snapshot: Option<u64>,
    /// Chunks to compare (default: whole world). One chunk keeps the
    /// legacy single-chunk output; several switch to grouped output.
    #[command(flatten)]
    pub selection: Selection,
    /// Human or JSON rendering plus optional phase timings.
    #[command(flatten)]
    pub output: TimingArgs,
    /// Show concrete old/new values in human output (SNBT format).
    #[arg(long)]
    pub show_values: bool,
}

/// World-portion selection shared by backup, rollback, and diff.
///
/// Exactly one of `--chunk`, `--region`, `--dimension` may be given;
/// none selects the whole world.
#[derive(Debug, Clone, clap::Args)]
pub struct Selection {
    /// Dimension namespace for `--chunk`/`--region`, or the whole
    /// dimension with `--dimension`.
    #[arg(long, default_value = "overworld")]
    pub dim: Dimension,
    /// Region family for `--chunk`/`--region`.
    #[arg(long, default_value = "region")]
    pub kind: RegionKind,
    /// Chunk `X,Z` in `--dim`/`--kind` (repeatable).
    #[arg(long, value_name = "X,Z", allow_hyphen_values = true, conflicts_with_all = ["region", "dimension"])]
    pub chunk: Vec<Xz>,
    /// Region `RX,RZ` in `--dim`/`--kind`: every known chunk in the file.
    #[arg(long, value_name = "RX,RZ", allow_hyphen_values = true, conflicts_with_all = ["chunk", "dimension"])]
    pub region: Option<Xz>,
    /// Whole `--dim` dimension, all region families.
    #[arg(long, conflicts_with_all = ["chunk", "region"])]
    pub dimension: bool,
}

impl Selection {
    /// Explicit chunk list in `--dim`/`--kind`.
    pub fn chunks(&self) -> Vec<ChunkCoord> {
        self.chunk
            .iter()
            .map(|xz| ChunkCoord::new(self.dim, self.kind, xz.x, xz.z))
            .collect()
    }

    /// Region identity for `--region`, if given.
    pub fn region_key(&self) -> Option<RegionKey> {
        self.region
            .map(|xz| RegionKey::new(self.dim, self.kind, xz.x, xz.z))
    }
}

/// `X,Z` coordinate pair for `--chunk` and `--region`.
#[derive(Debug, Clone, Copy)]
pub struct Xz {
    pub x: i32,
    pub z: i32,
}

impl std::str::FromStr for Xz {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (x, z) = s
            .split_once(',')
            .ok_or_else(|| format!("expected X,Z, got {s:?}"))?;
        let parse = |part: &str, axis: char| {
            part.trim()
                .parse()
                .map_err(|_| format!("invalid {axis} coordinate {part:?} in {s:?}, expected X,Z"))
        };
        Ok(Self {
            x: parse(x, 'X')?,
            z: parse(z, 'Z')?,
        })
    }
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
