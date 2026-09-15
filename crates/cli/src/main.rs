//! Thin `sekai` binary over `sekai-app`.
//!
//! Argument parsing and output formatting only. Quiesce the server before
//! snapshotting (e.g. `save-off`, `save-all`, then `save-on` afterwards);
//! orchestration belongs to the caller, never to this tool.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use sekai_app::{
    BackupOptions, BackupTimings, ChunkCoord, ChunkDiff, Dimension, GcPlan, GcReport, GcTimings,
    NbtChange, RegionKey, RegionKind, RollbackReport, RollbackTimings, Scope, SnapshotId,
};
use serde::Serialize;

mod style;

use style::{ColorChoice, Styler};

/// Chunk-level deduplicated snapshots for Minecraft region files.
#[derive(Debug, Parser)]
#[command(name = "sekai", version, about)]
struct Cli {
    /// Backup store directory (created when missing). Prefix with
    /// `sqlite://` explicitly if preferred.
    #[arg(long, global = true, default_value = "sekai-store")]
    store: String,

    /// Colorize human output. JSON output is never colorized.
    #[arg(long, global = true, value_enum, default_value_t = ColorChoice::Auto)]
    color: ColorChoice,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Record the current world state as a new snapshot.
    Backup {
        /// World directory (the one containing `region/`, `DIM-1/`, or `dimensions/`).
        /// For Bukkit-family servers, pass the server root instead.
        world: PathBuf,
        /// Print a per-phase timing breakdown after the report.
        #[arg(long)]
        timing: bool,
        /// Emit report as JSON instead of human text. Combined with
        /// `--timing`, phase timings are included. See docs/json.md.
        #[arg(long)]
        json: bool,
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
        /// Print a per-phase timing breakdown after the report.
        #[arg(long)]
        timing: bool,
        /// Emit report as JSON instead of human text. Combined with
        /// `--timing`, phase timings are included. See docs/json.md.
        #[arg(long)]
        json: bool,
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
        /// Print a per-phase timing breakdown after the report.
        #[arg(long)]
        timing: bool,
        /// Emit report as JSON instead of human text. Combined with
        /// `--timing`, phase timings are included. See docs/json.md.
        #[arg(long)]
        json: bool,
    },
    /// Read-only inspection helpers (never write to world or store).
    Debug {
        #[command(subcommand)]
        debug: DebugCommand,
    },
}

#[derive(Debug, clap::Args)]
struct DiffArgs {
    /// World directory to compare against snapshot (if specified).
    #[arg(long)]
    world: Option<PathBuf>,
    /// Older snapshot ID (if omitted when comparing snapshots, defaults to second-latest).
    old_snapshot: Option<u64>,
    /// Newer snapshot ID (if omitted, defaults to latest snapshot).
    new_snapshot: Option<u64>,
    /// Chunks to compare (default: whole world). One chunk keeps the
    /// legacy single-chunk output; several switch to grouped output.
    #[command(flatten)]
    selection: Selection,
    /// Emit diff array as JSON instead of human text.
    #[arg(long)]
    json: bool,
    /// Show concrete old/new values in human output (SNBT format).
    #[arg(long)]
    show_values: bool,
}

/// World-portion selection shared by backup, rollback, and diff.
///
/// Exactly one of `--chunk`, `--region`, `--dimension` may be given;
/// none selects the whole world.
#[derive(Debug, Clone, clap::Args)]
struct Selection {
    /// Dimension namespace for `--chunk`/`--region`, or the whole
    /// dimension with `--dimension`.
    #[arg(long, default_value = "overworld")]
    dim: Dimension,
    /// Region family for `--chunk`/`--region`.
    #[arg(long, default_value = "region")]
    kind: RegionKind,
    /// Chunk `X,Z` in `--dim`/`--kind` (repeatable).
    #[arg(long, value_name = "X,Z", allow_hyphen_values = true, conflicts_with_all = ["region", "dimension"])]
    chunk: Vec<Xz>,
    /// Region `RX,RZ` in `--dim`/`--kind`: every known chunk in the file.
    #[arg(long, value_name = "RX,RZ", allow_hyphen_values = true, conflicts_with_all = ["chunk", "dimension"])]
    region: Option<Xz>,
    /// Whole `--dim` dimension, all region families.
    #[arg(long, conflicts_with_all = ["chunk", "region"])]
    dimension: bool,
}

/// `X,Z` coordinate pair for `--chunk` and `--region`.
#[derive(Debug, Clone, Copy)]
struct Xz {
    x: i32,
    z: i32,
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

impl Selection {
    /// Explicit chunk list in `--dim`/`--kind`.
    fn chunks(&self) -> Vec<ChunkCoord> {
        self.chunk
            .iter()
            .map(|xz| ChunkCoord::new(self.dim, self.kind, xz.x, xz.z))
            .collect()
    }

    /// Region identity for `--region`, if given.
    fn region_key(&self) -> Option<RegionKey> {
        self.region
            .map(|xz| RegionKey::new(self.dim, self.kind, xz.x, xz.z))
    }
}

#[derive(Debug, Subcommand)]
enum DebugCommand {
    /// List region files with size, mtime, chunk count, and header hash.
    ///
    /// Read-only: never writes to the world or the store.
    Scan {
        /// World directory to inspect.
        world: PathBuf,
        /// Emit the entries as a JSON array instead of a table.
        #[arg(long)]
        json: bool,
    },
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let style = Styler::new(cli.color);
    let command_name = command_name(&cli.command);
    let as_json = output_json(&cli.command);
    let result = match cli.command {
        Command::Backup {
            world,
            timing,
            json,
            with_diff,
            jobs,
            selection,
        } => {
            let out = ReportOut {
                timing,
                json,
                style,
            };
            run_backup(&cli.store, &world, with_diff, jobs, selection, out).await
        }
        Command::Rollback {
            world,
            snapshot,
            timing,
            json,
            selection,
        } => {
            let out = ReportOut {
                timing,
                json,
                style,
            };
            run_rollback(&cli.store, &world, snapshot, selection, out).await
        }
        Command::List { json } => run_list(&cli.store, json, style).await,
        Command::Diff(args) => run_diff(&cli.store, args, style).await,
        Command::Gc {
            dry_run,
            timing,
            json,
        } => {
            let out = ReportOut {
                timing,
                json,
                style,
            };
            run_gc(&cli.store, dry_run, out).await
        }
        Command::Debug { debug } => match debug {
            DebugCommand::Scan { world, json } => run_debug_scan(&world, json),
        },
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            if as_json {
                // Envelope serialization is infallible for these shapes
                // (strings, integers, and structs thereof); the human
                // rendering is a last-resort fallback, never a second schema.
                match envelope_err(command_name, &err) {
                    Ok(doc) => println!("{doc}"),
                    Err(_) => {
                        eprintln!("{}", render_error(&err, Styler::new_stderr(cli.color)));
                    }
                }
            } else {
                eprintln!("{}", render_error(&err, Styler::new_stderr(cli.color)));
            }
            std::process::ExitCode::FAILURE
        }
    }
}

/// Output controls shared by report commands (backup, rollback, gc).
#[derive(Debug, Clone, Copy)]
struct ReportOut {
    timing: bool,
    json: bool,
    style: Styler,
}

/// Stable command name for JSON envelopes and error objects.
const fn command_name(command: &Command) -> &'static str {
    match command {
        Command::Backup { .. } => "backup",
        Command::Rollback { .. } => "rollback",
        Command::List { .. } => "list",
        Command::Diff(_) => "diff",
        Command::Gc { .. } => "gc",
        Command::Debug { debug } => match debug {
            DebugCommand::Scan { .. } => "scan",
        },
    }
}

/// Whether the command runs in JSON mode (`--json`). Errors serialize as
/// an envelope on stdout instead of styled text on stderr.
const fn output_json(command: &Command) -> bool {
    match command {
        Command::Backup { json, .. }
        | Command::Rollback { json, .. }
        | Command::List { json, .. }
        | Command::Gc { json, .. } => *json,
        Command::Diff(args) => args.json,
        Command::Debug { debug } => match debug {
            DebugCommand::Scan { json, .. } => *json,
        },
    }
}

/// JSON success envelope: exactly one JSON document on stdout.
#[derive(Serialize)]
struct OkEnvelope<'a, T: ?Sized> {
    command: &'a str,
    status: &'a str,
    result: &'a T,
}

fn envelope_ok<T: Serialize + ?Sized>(command: &str, result: &T) -> anyhow::Result<String> {
    Ok(serde_json::to_string(&OkEnvelope {
        command,
        status: "ok",
        result,
    })?)
}

/// JSON error object: stdout stays exactly one JSON document.
/// Callers must still check the exit code; `error` is only present here.
#[derive(Serialize)]
struct ErrEnvelope<'a> {
    command: &'a str,
    status: &'a str,
    error: String,
}

fn envelope_err(command: &str, err: &anyhow::Error) -> anyhow::Result<String> {
    Ok(serde_json::to_string(&ErrEnvelope {
        command,
        status: "error",
        error: format!("{err:?}"),
    })?)
}

/// Machine-readable payloads (docs/json.md). DTOs, not domain types: the
/// wire shape stays stable while domain structs evolve, durations convert
/// to integer millis at construction, and paths lossy-render up front.
/// Serde derives live only here; `no_std` crates never see `serde_json`.
#[derive(Serialize)]
struct BackupPayload {
    snapshot: u64,
    chunks: usize,
    new_blobs: usize,
    tombstones: usize,
    skipped_regions: usize,
    carried_chunks: usize,
    #[serde(flatten)]
    timing: Option<BackupTiming>,
}

#[derive(Serialize)]
struct BackupTiming {
    total_ms: u128,
    phases: BackupPhases,
    regions: Vec<RegionTimingJson>,
}

/// Phase breakdown for `backup --timing`. Field names are wire format
/// (docs/json.md), hence the uniform `_ms` postfix.
#[allow(clippy::struct_field_names)]
#[derive(Serialize)]
struct BackupPhases {
    discover_ms: u128,
    universe_load_ms: u128,
    fingerprint_ms: u128,
    region_open_ms: u128,
    ingest_ms: u128,
    hash_ms: u128,
    cas_put_ms: u128,
    db_apply_ms: u128,
}

#[derive(Serialize)]
struct RegionTimingJson {
    path: String,
    bytes: u64,
    chunks: usize,
    open_ms: u128,
    ingest_ms: u128,
    hash_ms: u128,
    cas_ms: u128,
}

#[derive(Serialize)]
struct RollbackPayload {
    files_written: usize,
    files_deleted: usize,
    chunks_restored: usize,
    #[serde(flatten)]
    timing: Option<RollbackTiming>,
}

#[derive(Serialize)]
struct RollbackTiming {
    total_ms: u128,
    phases: RollbackPhases,
}

/// Phase breakdown for `rollback --timing` (wire format, see above).
#[allow(clippy::struct_field_names)]
#[derive(Serialize)]
struct RollbackPhases {
    plan_ms: u128,
    discover_ms: u128,
    rollback_files_ms: u128,
}

#[derive(Serialize)]
struct GcPayload {
    candidates: usize,
    orphans: usize,
    removed: usize,
    #[serde(flatten)]
    timing: Option<GcTiming>,
}

#[derive(Serialize)]
struct GcTiming {
    total_ms: u128,
    phases: GcPhases,
}

/// Phase breakdown for `gc --timing` (wire format, see above).
#[allow(clippy::struct_field_names)]
#[derive(Serialize)]
struct GcPhases {
    plan_ms: u128,
    apply_ms: u128,
}

#[derive(Serialize)]
struct GcPlanPayload {
    orphans: usize,
    examined: usize,
}

#[derive(Serialize)]
struct SnapshotJson {
    id: u64,
    created_at_ms: u64,
}

/// Diff entry with SNBT string values. Deliberately not the derived
/// `Value` serialization (externally tagged): consumers read SNBT, so
/// values render through `Display` here, exactly like human output.
#[derive(Serialize)]
struct DiffEntryJson {
    path: String,
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    val: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    old: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new: Option<String>,
}

#[derive(Serialize)]
struct CoordJson {
    dim: i32,
    kind: i32,
    x: i32,
    z: i32,
}

#[derive(Serialize)]
struct DiffGroupJson {
    coord: CoordJson,
    entries: Vec<DiffEntryJson>,
}

#[derive(Serialize)]
struct ScanEntryJson {
    path: String,
    dim: i32,
    kind: i32,
    region_x: i32,
    region_z: i32,
    size: u64,
    mtime_ms: Option<u64>,
    chunks: usize,
    header_hash: String,
}

/// Render a runtime failure for stderr: red bold `error:` prefix plus the
/// anyhow context chain. Mirrors clap's own parse-error look.
fn render_error(err: &anyhow::Error, style: Styler) -> String {
    let chain = format!("{err:?}");
    let mut lines = chain.lines();
    let mut out = lines.next().map_or_else(
        || style.red_bold("error"),
        |first| format!("{}: {first}", style.red_bold("error")),
    );
    for line in lines {
        out.push('\n');
        out.push_str(line);
    }
    out
}

const fn options(with_diff: bool, jobs: usize) -> BackupOptions {
    BackupOptions {
        concurrency: jobs,
        with_diff,
        ignore_tags: None,
    }
}

async fn run_backup(
    store: &str,
    world: &Path,
    with_diff: bool,
    jobs: usize,
    selection: Selection,
    out: ReportOut,
) -> anyhow::Result<()> {
    let selected = selection.chunks();
    let scope = if selection.dimension {
        Scope::Dimension(selection.dim)
    } else if let Some(key) = selection.region_key() {
        Scope::Region(key)
    } else if selected.is_empty() {
        Scope::World
    } else {
        Scope::Chunks(&selected)
    };
    let (report, timings) =
        sekai_app::backup(world, store, options(with_diff, jobs), scope, |_| {})
            .await
            .with_context(|| format!("backup of {} failed", world.display()))?;
    if out.json {
        println!(
            "{}",
            envelope_ok("backup", &backup_payload(&report, &timings, out.timing))?
        );
        return Ok(());
    }
    let style = out.style;
    println!(
        "snapshot {} recorded: {} chunks, {} new blobs, {} tombstones",
        style.bold(&report.snapshot.raw().to_string()),
        report.chunks,
        report.new_blobs,
        report.tombstones
    );
    if out.timing {
        print_timing_table(&timings, style);
    }
    Ok(())
}

async fn run_rollback(
    store: &str,
    world: &Path,
    snapshot: u64,
    selection: Selection,
    out: ReportOut,
) -> anyhow::Result<()> {
    let id = SnapshotId(snapshot);
    let selected = selection.chunks();
    let scope = if selection.dimension {
        Scope::Dimension(selection.dim)
    } else if let Some(key) = selection.region_key() {
        Scope::Region(key)
    } else if selected.is_empty() {
        Scope::World
    } else {
        Scope::Chunks(&selected)
    };
    let (report, timings) = sekai_app::rollback(world, store, id, scope)
        .await
        .with_context(|| {
            format!(
                "rollback of {} to snapshot {snapshot} failed",
                world.display()
            )
        })?;
    if out.json {
        println!(
            "{}",
            envelope_ok("rollback", &rollback_payload(&report, &timings, out.timing))?
        );
        return Ok(());
    }
    let style = out.style;
    println!(
        "snapshot {} restored: {} files rewritten, {} files deleted, {} chunks restored",
        style.bold(&snapshot.to_string()),
        report.files_written,
        report.files_deleted,
        report.chunks_restored
    );
    if out.timing {
        print_rollback_timing_table(&timings, style);
    }
    Ok(())
}

async fn run_list(store: &str, json: bool, style: Styler) -> anyhow::Result<()> {
    let snapshots = sekai_app::list_snapshots(store).await?;
    if json {
        println!("{}", envelope_ok("list", &list_payload(&snapshots))?);
        return Ok(());
    }
    for snapshot in &snapshots {
        println!(
            "{}\t{}",
            style.bold(&snapshot.id.raw().to_string()),
            format_time(snapshot.created_at_ms)
        );
    }
    Ok(())
}

/// Human-readable diff lines. Values are SNBT, truncated per value so a
/// single huge entry cannot flood the terminal; JSON output always carries
/// full values.
fn render_diff_human(
    cx: i32,
    cz: i32,
    diffs: &[sekai_app::NbtDiffEntry],
    show_values: bool,
    style: Styler,
) -> String {
    if diffs.is_empty() {
        return format!("No differences found for chunk ({cx}, {cz}).");
    }
    render_diff_entries(diffs, show_values, style)
}

/// Human-readable diff sections for several chunks; callers omit chunks
/// without entries, so every section is non-empty.
fn render_diff_grouped(diffs: &[&ChunkDiff], show_values: bool, style: Styler) -> String {
    diffs
        .iter()
        .map(|diff| {
            let coord = diff.coord;
            let header = format!(
                "chunk {}/{} ({}, {}):",
                coord.dim, coord.kind, coord.x, coord.z
            );
            format!(
                "{}\n{}",
                style.bold(&header),
                render_diff_entries(&diff.entries, show_values, style)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One diff entry per line, shared by single-chunk and grouped rendering.
fn render_diff_entries(
    diffs: &[sekai_app::NbtDiffEntry],
    show_values: bool,
    style: Styler,
) -> String {
    diffs
        .iter()
        .map(|entry| {
            let (op, detail) = match &entry.change {
                NbtChange::Added(v) => (style.green("+"), show_values.then(|| format!("{v}"))),
                NbtChange::Removed(v) => (style.red("-"), show_values.then(|| format!("{v}"))),
                NbtChange::Modified { old, new } => (
                    style.yellow("~"),
                    show_values.then(|| format!("{old} -> {new}")),
                ),
            };
            detail.map_or_else(
                || format!("{op} {}", entry.path),
                |detail| format!("{op} {}: {}", entry.path, truncate_value(&detail)),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Truncate a rendered value. Paths are never truncated.
fn truncate_value(rendered: &str) -> String {
    const MAX_VALUE_CHARS: usize = 500;
    if rendered.chars().count() <= MAX_VALUE_CHARS {
        return rendered.to_owned();
    }
    let truncated: String = rendered.chars().take(MAX_VALUE_CHARS).collect();
    format!("{truncated}...")
}

async fn run_diff(store: &str, args: DiffArgs, style: Styler) -> anyhow::Result<()> {
    let selection = &args.selection;
    let explicit = selection.chunks();

    let diffs = if let Some(world) = &args.world {
        let snapshot_id = args.old_snapshot.or(args.new_snapshot).map(SnapshotId);
        let coords = if explicit.is_empty() {
            scoped_coords(sekai_app::world_chunk_coords(world)?, selection)
        } else {
            explicit
        };
        sekai_app::diff_world_chunks(world, store, snapshot_id, &coords, None)
            .await
            .with_context(|| "failed to compute chunk diffs between world state and snapshot")?
    } else {
        let (old_id, new_id) =
            resolve_snapshot_pair(store, args.old_snapshot, args.new_snapshot).await?;
        let coords = if explicit.is_empty() {
            let mut union = sekai_app::snapshot_chunk_coords(store, old_id).await?;
            union.extend(sekai_app::snapshot_chunk_coords(store, new_id).await?);
            scoped_coords(union, selection)
        } else {
            explicit
        };
        sekai_app::diff_chunks(store, old_id, new_id, &coords, None)
            .await
            .with_context(|| {
                format!(
                    "failed to compute chunk diffs between snapshot {} and {}",
                    old_id.raw(),
                    new_id.raw()
                )
            })?
    };

    // Exactly one chunk keeps the legacy single-chunk output; several
    // switch to grouped output with empty diffs omitted.
    if diffs.len() == 1 {
        let diff = &diffs[0];
        if args.json {
            println!(
                "{}",
                envelope_ok("diff", &diff_entry_payload(&diff.entries))?
            );
            return Ok(());
        }
        println!(
            "{}",
            render_diff_human(
                diff.coord.x,
                diff.coord.z,
                &diff.entries,
                args.show_values,
                style
            )
        );
        return Ok(());
    }

    let nonempty: Vec<&ChunkDiff> = diffs
        .iter()
        .filter(|diff| !diff.entries.is_empty())
        .collect();
    if args.json {
        println!("{}", envelope_ok("diff", &diff_group_payload(&nonempty))?);
        return Ok(());
    }
    if nonempty.is_empty() {
        println!("No differences found.");
        return Ok(());
    }
    println!(
        "{}",
        render_diff_grouped(&nonempty, args.show_values, style)
    );
    Ok(())
}

/// Resolve the snapshot pair, defaulting omitted IDs to latest/second-latest.
async fn resolve_snapshot_pair(
    store: &str,
    old_snapshot: Option<u64>,
    new_snapshot: Option<u64>,
) -> anyhow::Result<(SnapshotId, SnapshotId)> {
    match (old_snapshot, new_snapshot) {
        (Some(old), Some(new)) => Ok((SnapshotId(old), SnapshotId(new))),
        (Some(old), None) => {
            let latest = sekai_app::latest_snapshot_id(store).await?;
            Ok((SnapshotId(old), latest))
        }
        (None, None) => {
            let snapshots = sekai_app::list_snapshots(store).await?;
            if snapshots.len() < 2 {
                anyhow::bail!("at least 2 snapshots are required when snapshot IDs are omitted");
            }
            let old = snapshots[snapshots.len() - 2].id;
            let new = snapshots[snapshots.len() - 1].id;
            Ok((old, new))
        }
        (None, Some(new)) => {
            let snapshots = sekai_app::list_snapshots(store).await?;
            if snapshots.len() < 2 {
                anyhow::bail!("at least 2 snapshots are required when snapshot IDs are omitted");
            }
            let old = snapshots[snapshots.len() - 2].id;
            Ok((old, SnapshotId(new)))
        }
    }
}

/// Keep coordinates inside `--region`/`--dimension` (explicit `--chunk`
/// lists bypass this), sorted and deduplicated.
fn scoped_coords(mut coords: Vec<ChunkCoord>, selection: &Selection) -> Vec<ChunkCoord> {
    if let Some(key) = selection.region_key() {
        coords.retain(|coord| RegionKey::of(*coord) == key);
    } else if selection.dimension {
        coords.retain(|coord| coord.dim == selection.dim);
    }
    coords.sort();
    coords.dedup();
    coords
}

async fn run_gc(store: &str, dry_run: bool, out: ReportOut) -> anyhow::Result<()> {
    let style = out.style;
    if dry_run {
        let plan = sekai_app::gc_plan(store)
            .await
            .with_context(|| format!("gc plan for {store} failed"))?;
        if out.json {
            println!("{}", envelope_ok("gc", &gc_plan_payload(&plan))?);
            return Ok(());
        }
        println!(
            "gc plan created: {} orphan blobs (examined {})",
            highlight_count(style, plan.orphans.len()),
            plan.examined
        );
        return Ok(());
    }

    let (report, timings) = sekai_app::gc(store)
        .await
        .with_context(|| format!("gc for {store} failed"))?;
    if out.json {
        println!(
            "{}",
            envelope_ok("gc", &gc_payload(&report, &timings, out.timing))?
        );
        return Ok(());
    }
    println!(
        "gc completed: {} removed, {} orphans, {} candidates",
        highlight_count(style, report.removed),
        report.orphans,
        report.candidates
    );
    if out.timing {
        print_gc_timing_table(&timings, style);
    }
    Ok(())
}

/// Emphasize nonzero counts (work done or pending); zero stays plain.
fn highlight_count(style: Styler, count: usize) -> String {
    let text = count.to_string();
    if count == 0 {
        text
    } else {
        style.yellow(&text)
    }
}

/// Unix millis to RFC 3339, falling back to the raw number when absurd.
fn format_time(created_at_ms: u64) -> String {
    let millis = i64::try_from(created_at_ms).unwrap_or(i64::MAX);
    chrono::DateTime::from_timestamp_millis(millis)
        .map_or_else(|| format!("{created_at_ms}ms"), |time| time.to_rfc3339())
}

/// Human-readable phase table for `backup --timing`.
fn print_timing_table(timings: &BackupTimings, style: Styler) {
    println!(
        "{}",
        style.dim(&format!(
            "timing total={}ms discover={}ms universe={}ms fp={}ms open={}ms ingest={}ms (hash={}ms cas={}ms) db={}ms ingested_files={} skipped_files={} carried_chunks={}",
            timings.total.as_millis(),
            timings.discover.as_millis(),
            timings.universe_load.as_millis(),
            timings.fingerprint.as_millis(),
            timings.region_open.as_millis(),
            timings.ingest.as_millis(),
            timings.hash.as_millis(),
            timings.cas_put.as_millis(),
            timings.db_apply.as_millis(),
            timings.regions.len(),
            timings.skipped_regions,
            timings.carried_chunks,
        ))
    );
    let mut slowest: Vec<&sekai_app::RegionTiming> = timings.regions.iter().collect();
    slowest.sort_by_key(|r| std::cmp::Reverse((r.open + r.ingest).as_micros()));
    for region in slowest.iter().take(5) {
        println!(
            "{}",
            style.dim(&format!(
                "  {} chunks={} bytes={} open={}ms ingest={}ms (hash={}ms cas={}ms)",
                region.path.display(),
                region.chunks,
                region.bytes,
                region.open.as_millis(),
                region.ingest.as_millis(),
                region.hash.as_millis(),
                region.cas.as_millis(),
            ))
        );
    }
}

/// Human-readable phase table for `rollback --timing`.
fn print_rollback_timing_table(timings: &RollbackTimings, style: Styler) {
    println!(
        "{}",
        style.dim(&format!(
            "timing total={}ms plan={}ms discover={}ms rollback_files={}ms",
            timings.total.as_millis(),
            timings.plan.as_millis(),
            timings.discover.as_millis(),
            timings.rollback_files.as_millis(),
        ))
    );
}

/// Human-readable phase table for `gc --timing`.
fn print_gc_timing_table(timings: &GcTimings, style: Styler) {
    println!(
        "{}",
        style.dim(&format!(
            "timing total={}ms plan={}ms apply={}ms",
            timings.total.as_millis(),
            timings.plan.as_millis(),
            timings.apply.as_millis(),
        ))
    );
}

/// Grouped payload for multi-chunk diffs. `coord` uses raw dim/kind codes,
/// matching `scan`; entry values are SNBT strings, always complete.
fn diff_group_payload(diffs: &[&ChunkDiff]) -> Vec<DiffGroupJson> {
    diffs
        .iter()
        .map(|diff| DiffGroupJson {
            coord: CoordJson {
                dim: diff.coord.dim.raw(),
                kind: diff.coord.kind.raw(),
                x: diff.coord.x,
                z: diff.coord.z,
            },
            entries: diff_entry_payload(&diff.entries),
        })
        .collect()
}

/// Payload for single-chunk `diff --json`. Values are SNBT strings.
fn diff_entry_payload(diffs: &[sekai_app::NbtDiffEntry]) -> Vec<DiffEntryJson> {
    diffs.iter().map(diff_entry_json).collect()
}

fn diff_entry_json(entry: &sekai_app::NbtDiffEntry) -> DiffEntryJson {
    let (kind, val, old, new) = match &entry.change {
        NbtChange::Added(v) => ("added", Some(format!("{v}")), None, None),
        NbtChange::Removed(v) => ("removed", Some(format!("{v}")), None, None),
        NbtChange::Modified { old, new } => (
            "modified",
            None,
            Some(format!("{old}")),
            Some(format!("{new}")),
        ),
    };
    DiffEntryJson {
        path: entry.path.clone(),
        kind,
        val,
        old,
        new,
    }
}

/// Payload for `backup --json`. Without `--timing` this is the bare
/// result; with `--timing` the timing block is included, mirroring the
/// human `--timing` table plus the full region list.
fn backup_payload(
    report: &sekai_app::BackupReport,
    timings: &BackupTimings,
    timing: bool,
) -> BackupPayload {
    BackupPayload {
        snapshot: report.snapshot.raw(),
        chunks: report.chunks,
        new_blobs: report.new_blobs,
        tombstones: report.tombstones,
        skipped_regions: report.skipped_regions,
        carried_chunks: report.carried_chunks,
        timing: timing.then_some(BackupTiming {
            total_ms: timings.total.as_millis(),
            phases: BackupPhases {
                discover_ms: timings.discover.as_millis(),
                universe_load_ms: timings.universe_load.as_millis(),
                fingerprint_ms: timings.fingerprint.as_millis(),
                region_open_ms: timings.region_open.as_millis(),
                ingest_ms: timings.ingest.as_millis(),
                hash_ms: timings.hash.as_millis(),
                cas_put_ms: timings.cas_put.as_millis(),
                db_apply_ms: timings.db_apply.as_millis(),
            },
            regions: timings
                .regions
                .iter()
                .map(|region| RegionTimingJson {
                    path: region.path.to_string_lossy().into_owned(),
                    bytes: region.bytes,
                    chunks: region.chunks,
                    open_ms: region.open.as_millis(),
                    ingest_ms: region.ingest.as_millis(),
                    hash_ms: region.hash.as_millis(),
                    cas_ms: region.cas.as_millis(),
                })
                .collect(),
        }),
    }
}

/// Payload for `rollback --json`; the timing block needs `--timing`.
fn rollback_payload(
    report: &RollbackReport,
    timings: &RollbackTimings,
    timing: bool,
) -> RollbackPayload {
    RollbackPayload {
        files_written: report.files_written,
        files_deleted: report.files_deleted,
        chunks_restored: report.chunks_restored,
        timing: timing.then_some(RollbackTiming {
            total_ms: timings.total.as_millis(),
            phases: RollbackPhases {
                plan_ms: timings.plan.as_millis(),
                discover_ms: timings.discover.as_millis(),
                rollback_files_ms: timings.rollback_files.as_millis(),
            },
        }),
    }
}

/// Payload for `gc --json`; the timing block needs `--timing`.
/// The dry-run plan has its own shape, see [`gc_plan_payload`], since
/// planning produces no phase timings.
fn gc_payload(report: &GcReport, timings: &GcTimings, timing: bool) -> GcPayload {
    GcPayload {
        candidates: report.candidates,
        orphans: report.orphans,
        removed: report.removed,
        timing: timing.then_some(GcTiming {
            total_ms: timings.total.as_millis(),
            phases: GcPhases {
                plan_ms: timings.plan.as_millis(),
                apply_ms: timings.apply.as_millis(),
            },
        }),
    }
}

/// Payload for `gc --dry-run --json`. No phase timings exist for a plan,
/// so this shape never carries a timing block.
const fn gc_plan_payload(plan: &GcPlan) -> GcPlanPayload {
    GcPlanPayload {
        orphans: plan.orphans.len(),
        examined: plan.examined,
    }
}

fn run_debug_scan(world: &Path, json: bool) -> anyhow::Result<()> {
    let entries =
        sekai_app::scan(world).with_context(|| format!("scan of {} failed", world.display()))?;
    if json {
        println!("{}", envelope_ok("scan", &scan_payload(&entries))?);
        return Ok(());
    }
    for entry in &entries {
        let short_hash = entry
            .header_hash
            .char_indices()
            .nth(12)
            .map_or(entry.header_hash.as_str(), |(idx, _)| {
                &entry.header_hash[..idx]
            });

        println!(
            "{} dim={} kind={} region=r.{}.{} size={} mtime={} chunks={} header={}",
            entry.path.display(),
            entry.dim.raw(),
            kind_name(entry.kind),
            entry.region_x,
            entry.region_z,
            entry.file_bytes,
            entry
                .mtime_ms
                .map_or_else(|| "n/a".to_owned(), |ms| ms.to_string()),
            entry.chunks,
            short_hash,
        );
    }
    let total_bytes: u64 = entries.iter().map(|e| e.file_bytes).sum();
    let total_chunks: usize = entries.iter().map(|e| e.chunks).sum();
    println!(
        "{} files, {} bytes, {} chunks",
        entries.len(),
        total_bytes,
        total_chunks
    );
    Ok(())
}

/// Flat JSON array for `list --json`: snapshot IDs with raw
/// millisecond timestamps (RFC 3339 rendering stays human-only).
/// Payload for `list --json`: snapshot IDs with raw Unix millis.
fn list_payload(snapshots: &[sekai_app::Snapshot]) -> Vec<SnapshotJson> {
    snapshots
        .iter()
        .map(|snapshot| SnapshotJson {
            id: snapshot.id.raw(),
            created_at_ms: snapshot.created_at_ms,
        })
        .collect()
}

/// Payload for `debug scan --json`.
fn scan_payload(entries: &[sekai_app::RegionScanEntry]) -> Vec<ScanEntryJson> {
    entries
        .iter()
        .map(|entry| ScanEntryJson {
            path: entry.path.to_string_lossy().into_owned(),
            dim: entry.dim.raw(),
            kind: entry.kind.raw(),
            region_x: entry.region_x,
            region_z: entry.region_z,
            size: entry.file_bytes,
            mtime_ms: entry.mtime_ms,
            chunks: entry.chunks,
            header_hash: entry.header_hash.clone(),
        })
        .collect()
}

/// Short family name for a region kind (`region`/`entities`/`poi`).
fn kind_name(kind: RegionKind) -> &'static str {
    if kind == RegionKind::REGION {
        "region"
    } else if kind == RegionKind::ENTITIES {
        "entities"
    } else if kind == RegionKind::POI {
        "poi"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::time::Duration;
    use std::path::PathBuf;

    /// Serialize a payload DTO, proving the exact wire bytes.
    fn json<T: Serialize + ?Sized>(value: &T) -> String {
        serde_json::to_string(value).unwrap()
    }

    #[test]
    fn parses_subcommands() {
        let cli = Cli::try_parse_from(["sekai", "backup", "world"]).expect("backup parses");
        assert!(matches!(cli.command, Command::Backup { .. }));
        assert_eq!(cli.store, "sekai-store");

        let cli = Cli::try_parse_from(["sekai", "--store", "s", "rollback", "w", "3"])
            .expect("rollback parses");
        assert!(matches!(cli.command, Command::Rollback { snapshot: 3, .. }));

        assert!(Cli::try_parse_from(["sekai", "rollback", "w"]).is_err());
    }

    #[test]
    fn parses_diff_command() {
        let cli = Cli::try_parse_from(["sekai", "diff", "1", "2", "--chunk", "10,-5"])
            .expect("diff parses");
        assert!(matches!(
            cli.command,
            Command::Diff(DiffArgs {
                world: None,
                old_snapshot: Some(1),
                new_snapshot: Some(2),
                selection: Selection {
                    chunk: _,
                    region: None,
                    dimension: false,
                    ..
                },
                json: false,
                show_values: false,
            })
        ));
        if let Command::Diff(args) = &cli.command {
            assert_eq!(args.selection.chunks().len(), 1);
            assert_eq!(args.selection.chunks()[0].x, 10);
        } else {
            panic!("expected diff");
        }

        let cli = Cli::try_parse_from(["sekai", "diff", "--world", "world", "--chunk", "10,-5"])
            .expect("diff with world parses");
        assert!(matches!(
            cli.command,
            Command::Diff(DiffArgs {
                world: Some(_),
                old_snapshot: None,
                new_snapshot: None,
                json: false,
                show_values: false,
                ..
            })
        ));

        let cli =
            Cli::try_parse_from(["sekai", "diff", "1", "2", "--chunk", "0,0", "--show-values"])
                .expect("diff --show-values parses");
        assert!(matches!(
            cli.command,
            Command::Diff(DiffArgs {
                show_values: true,
                ..
            })
        ));

        // Multi-chunk repeats, region/dimension selectors, mutual exclusion.
        let cli = Cli::try_parse_from(["sekai", "diff", "--chunk", "0,0", "--chunk", "1,-1"])
            .expect("repeated --chunk parses");
        if let Command::Diff(args) = &cli.command {
            assert_eq!(args.selection.chunks().len(), 2);
        } else {
            panic!("expected diff");
        }
        let cli = Cli::try_parse_from(["sekai", "diff", "--region", "0,0"]).expect("region parses");
        assert!(matches!(
            cli.command,
            Command::Diff(DiffArgs {
                selection: Selection {
                    region: Some(_),
                    dimension: false,
                    ..
                },
                ..
            })
        ));
        assert!(Cli::try_parse_from(["sekai", "diff", "--region", "0,0", "--dimension"]).is_err());
        assert!(
            Cli::try_parse_from(["sekai", "diff", "--chunk", "0,0", "--region", "0,0"]).is_err()
        );
        assert!(Cli::try_parse_from(["sekai", "diff", "--chunk", "0,0", "--dimension"]).is_err());
        assert!(Cli::try_parse_from(["sekai", "diff", "--chunk", "bogus"]).is_err());
        assert!(Cli::try_parse_from(["sekai", "diff", "--cx", "0"]).is_err());
    }

    #[test]
    fn parses_selection_for_backup_and_rollback() {
        let cli = Cli::try_parse_from(["sekai", "backup", "--dimension", "--dim", "nether", "w"])
            .expect("backup --dimension parses");
        assert!(matches!(
            cli.command,
            Command::Backup {
                selection: Selection {
                    dimension: true,
                    dim: Dimension::NETHER,
                    ..
                },
                ..
            }
        ));

        let cli = Cli::try_parse_from(["sekai", "rollback", "w", "3", "--region", "1,-1"])
            .expect("rollback --region parses");
        assert!(matches!(
            cli.command,
            Command::Rollback {
                snapshot: 3,
                selection: Selection {
                    region: Some(_),
                    ..
                },
                ..
            }
        ));

        assert!(
            Cli::try_parse_from(["sekai", "backup", "w", "--region", "0,0", "--dimension"])
                .is_err()
        );
    }

    #[test]
    fn parses_xz_pairs() {
        assert!(matches!(
            "10,-5".parse::<Xz>().expect("parses"),
            Xz { x: 10, z: -5 }
        ));
        assert!("bogus".parse::<Xz>().is_err());
        assert!("1".parse::<Xz>().is_err());
        assert!("1,2,3".parse::<Xz>().is_err());
        assert!("a,b".parse::<Xz>().is_err());
    }

    #[test]
    fn parses_timing_and_debug_scan() {
        let cli = Cli::try_parse_from(["sekai", "backup", "--timing", "world"])
            .expect("backup --timing parses");
        assert!(matches!(cli.command, Command::Backup { timing: true, .. }));

        // `--timing` and `--json` are orthogonal: the JSON report carries
        // the timing block when both are set.
        let cli = Cli::try_parse_from(["sekai", "backup", "--timing", "--json", "world"])
            .expect("backup --timing --json parses");
        assert!(matches!(
            cli.command,
            Command::Backup {
                timing: true,
                json: true,
                ..
            }
        ));

        let cli = Cli::try_parse_from(["sekai", "rollback", "--timing", "w", "3"])
            .expect("rollback --timing parses");
        assert!(matches!(
            cli.command,
            Command::Rollback { timing: true, .. }
        ));

        let cli = Cli::try_parse_from(["sekai", "rollback", "--json", "w", "3"])
            .expect("rollback --json parses");
        assert!(matches!(cli.command, Command::Rollback { json: true, .. }));

        let cli = Cli::try_parse_from(["sekai", "list", "--json"]).expect("list --json parses");
        assert!(matches!(cli.command, Command::List { json: true }));

        let cli =
            Cli::try_parse_from(["sekai", "debug", "scan", "world"]).expect("debug scan parses");
        assert!(matches!(cli.command, Command::Debug { .. }));
    }

    #[test]
    fn parses_gc_command() {
        let cli = Cli::try_parse_from(["sekai", "gc"]).expect("gc parses");
        assert!(matches!(
            cli.command,
            Command::Gc {
                dry_run: false,
                timing: false,
                json: false
            }
        ));

        let cli = Cli::try_parse_from(["sekai", "gc", "--dry-run", "--json"])
            .expect("gc --dry-run --json parses");
        assert!(matches!(
            cli.command,
            Command::Gc {
                dry_run: true,
                json: true,
                ..
            }
        ));

        // `--timing` and `--json` compose; no exclusivity between them.
        let cli = Cli::try_parse_from(["sekai", "gc", "--timing", "--json"])
            .expect("gc --timing --json parses");
        assert!(matches!(
            cli.command,
            Command::Gc {
                timing: true,
                json: true,
                ..
            }
        ));
    }

    #[test]
    fn formats_times() {
        assert_eq!(format_time(0), "1970-01-01T00:00:00+00:00");
        // Absurd values fall back instead of panicking.
        assert_eq!(format_time(u64::MAX), format!("{}ms", u64::MAX));
    }

    #[test]
    fn escapes_json_strings_like_serde() {
        // serde_json owns all string escaping now; pin parity for quotes,
        // backslashes, C0 controls (lowercase `\u00xx`), and passthrough.
        assert_eq!(json(&"a\"b\\c"), r#""a\"b\\c""#);
        assert_eq!(json(&"a\nb\rc\td"), r#""a\nb\rc\td""#);
        assert_eq!(json(&"a\x01b"), r#""a\u0001b""#);
        assert_eq!(json(&"日本語~"), r#""日本語~""#);
    }

    fn sample_diffs() -> Vec<sekai_app::NbtDiffEntry> {
        vec![
            sekai_app::NbtDiffEntry {
                path: "Status".to_owned(),
                change: NbtChange::Modified {
                    old: sekai_app::NbtValue::String("minecraft:full".to_owned()),
                    new: sekai_app::NbtValue::String("minecraft:empty".to_owned()),
                },
            },
            sekai_app::NbtDiffEntry {
                path: "xPos".to_owned(),
                change: NbtChange::Added(sekai_app::NbtValue::Int(3)),
            },
            sekai_app::NbtDiffEntry {
                path: "old_tag".to_owned(),
                change: NbtChange::Removed(sekai_app::NbtValue::Byte(1)),
            },
        ]
    }

    #[test]
    fn renders_diff_human_without_values() {
        assert_eq!(
            render_diff_human(10, -5, &sample_diffs(), false, Styler::disabled()),
            "~ Status\n+ xPos\n- old_tag"
        );
    }

    #[test]
    fn renders_diff_human_with_values() {
        assert_eq!(
            render_diff_human(10, -5, &sample_diffs(), true, Styler::disabled()),
            "~ Status: \"minecraft:full\" -> \"minecraft:empty\"\n+ xPos: 3\n- old_tag: 1b"
        );
    }

    #[test]
    fn renders_diff_human_styled() {
        assert_eq!(
            render_diff_human(10, -5, &sample_diffs(), false, Styler::enabled()),
            "\x1b[33m~\x1b[0m Status\n\x1b[32m+\x1b[0m xPos\n\x1b[31m-\x1b[0m old_tag"
        );
    }

    #[test]
    fn renders_diff_human_empty() {
        assert_eq!(
            render_diff_human(10, -5, &[], true, Styler::disabled()),
            "No differences found for chunk (10, -5)."
        );
    }

    #[test]
    fn truncates_huge_values() {
        let big = "x".repeat(600);
        let diffs = vec![sekai_app::NbtDiffEntry {
            path: "blob".to_owned(),
            change: NbtChange::Added(sekai_app::NbtValue::String(big)),
        }];
        let rendered = render_diff_human(0, 0, &diffs, true, Styler::disabled());
        assert!(rendered.starts_with("+ blob: \"xxx"));
        assert!(rendered.ends_with("..."));
        assert_eq!(
            rendered.chars().count(),
            "+ blob: ".len() + 500 + "...".len()
        );
        // JSON output is never truncated.
        assert!(json(&diff_entry_payload(&diffs)).contains(&"x".repeat(600)));
    }

    #[test]
    fn renders_diff_json_with_snbt_values() {
        assert_eq!(
            json(&diff_entry_payload(&sample_diffs())),
            r#"[{"path":"Status","type":"modified","old":"\"minecraft:full\"","new":"\"minecraft:empty\""},{"path":"xPos","type":"added","val":"3"},{"path":"old_tag","type":"removed","val":"1b"}]"#
        );
    }

    fn sample_grouped() -> Vec<sekai_app::ChunkDiff> {
        vec![
            sekai_app::ChunkDiff {
                coord: ChunkCoord::new(Dimension::OVERWORLD, RegionKind::REGION, 0, 0),
                entries: sample_diffs(),
            },
            sekai_app::ChunkDiff {
                coord: ChunkCoord::new(Dimension::NETHER, RegionKind::REGION, 5, -3),
                entries: Vec::new(),
            },
        ]
    }

    #[test]
    fn renders_diff_grouped_json() {
        let grouped = sample_grouped();
        let nonempty: Vec<&sekai_app::ChunkDiff> = grouped
            .iter()
            .filter(|diff| !diff.entries.is_empty())
            .collect();
        assert_eq!(
            json(&diff_group_payload(&nonempty)),
            r#"[{"coord":{"dim":0,"kind":0,"x":0,"z":0},"entries":[{"path":"Status","type":"modified","old":"\"minecraft:full\"","new":"\"minecraft:empty\""},{"path":"xPos","type":"added","val":"3"},{"path":"old_tag","type":"removed","val":"1b"}]}]"#
        );
        assert_eq!(json(&diff_group_payload(&[])), "[]");
    }

    #[test]
    fn renders_diff_grouped_human() {
        let grouped = sample_grouped();
        let nonempty: Vec<&sekai_app::ChunkDiff> = grouped
            .iter()
            .filter(|diff| !diff.entries.is_empty())
            .collect();
        assert_eq!(
            render_diff_grouped(&nonempty, false, Styler::disabled()),
            "chunk overworld/region (0, 0):\n~ Status\n+ xPos\n- old_tag"
        );
    }

    #[test]
    fn scoped_coords_filters_and_dedups() {
        let coords = vec![
            ChunkCoord::new(Dimension::OVERWORLD, RegionKind::REGION, 0, 0),
            ChunkCoord::new(Dimension::OVERWORLD, RegionKind::REGION, 0, 0),
            ChunkCoord::new(Dimension::OVERWORLD, RegionKind::REGION, 33, 0),
            ChunkCoord::new(Dimension::NETHER, RegionKind::REGION, 0, 0),
        ];
        let region = Selection {
            dim: Dimension::OVERWORLD,
            kind: RegionKind::REGION,
            chunk: Vec::new(),
            region: Some(Xz { x: 0, z: 0 }),
            dimension: false,
        };
        assert_eq!(
            scoped_coords(coords.clone(), &region),
            vec![ChunkCoord::new(
                Dimension::OVERWORLD,
                RegionKind::REGION,
                0,
                0
            )]
        );
        let dimension = Selection {
            dim: Dimension::NETHER,
            dimension: true,
            ..region.clone()
        };
        assert_eq!(
            scoped_coords(coords.clone(), &dimension),
            vec![ChunkCoord::new(Dimension::NETHER, RegionKind::REGION, 0, 0)]
        );
        let world = Selection {
            region: None,
            dimension: false,
            ..region
        };
        assert_eq!(scoped_coords(coords, &world).len(), 3);
    }

    #[test]
    fn parses_color_flag() {
        let cli = Cli::try_parse_from(["sekai", "backup", "world"]).expect("backup parses");
        assert_eq!(cli.color, ColorChoice::Auto);

        let cli = Cli::try_parse_from(["sekai", "--color=never", "list"]).expect("parses");
        assert_eq!(cli.color, ColorChoice::Never);

        let cli = Cli::try_parse_from(["sekai", "--color", "always", "list"]).expect("parses");
        assert_eq!(cli.color, ColorChoice::Always);

        assert!(Cli::try_parse_from(["sekai", "--color=maybe", "list"]).is_err());
    }

    #[test]
    fn reports_json_mode_per_command() {
        // JSON is per-command; a bare list is human, `--json` is machine.
        let cli = Cli::try_parse_from(["sekai", "list"]).expect("parses");
        assert!(!output_json(&cli.command));

        let cli = Cli::try_parse_from(["sekai", "list", "--json"]).expect("parses");
        assert!(output_json(&cli.command));

        let cli = Cli::try_parse_from(["sekai", "backup", "w"]).expect("parses");
        assert!(!output_json(&cli.command));

        let cli = Cli::try_parse_from(["sekai", "diff", "--chunk", "0,0"]).expect("parses");
        assert!(!output_json(&cli.command));

        let cli =
            Cli::try_parse_from(["sekai", "diff", "--chunk", "0,0", "--json"]).expect("parses");
        assert!(output_json(&cli.command));

        assert!(Cli::try_parse_from(["sekai", "backup", "--timing-json", "w"]).is_err());
    }

    #[test]
    fn renders_json_envelopes() {
        assert_eq!(
            envelope_ok("list", &serde_json::json!([1, 2])).unwrap(),
            r#"{"command":"list","status":"ok","result":[1,2]}"#
        );
        let err = anyhow::anyhow!("root cause").context("backup of /w failed");
        assert_eq!(
            envelope_err("backup", &err).unwrap(),
            r#"{"command":"backup","status":"error","error":"backup of /w failed\n\nCaused by:\n    root cause"}"#
        );
    }

    #[test]
    fn renders_list_json() {
        let empty: Vec<sekai_app::Snapshot> = Vec::new();
        assert_eq!(json(&list_payload(&empty)), "[]");
        let snapshots = vec![
            sekai_app::Snapshot {
                id: sekai_app::SnapshotId(1),
                created_at_ms: 1_700_000_000_000,
            },
            sekai_app::Snapshot {
                id: sekai_app::SnapshotId(2),
                created_at_ms: 1_700_000_001_000,
            },
        ];
        assert_eq!(
            json(&list_payload(&snapshots)),
            r#"[{"id":1,"created_at_ms":1700000000000},{"id":2,"created_at_ms":1700000001000}]"#
        );
    }

    #[test]
    fn renders_report_json_without_and_with_timings() {
        let backup_report = sekai_app::BackupReport {
            snapshot: sekai_app::SnapshotId(3),
            chunks: 40,
            new_blobs: 2,
            tombstones: 0,
            skipped_regions: 1,
            carried_chunks: 8,
        };
        let backup_timings = BackupTimings {
            total: Duration::from_millis(100),
            discover: Duration::from_millis(1),
            universe_load: Duration::from_millis(2),
            fingerprint: Duration::from_millis(3),
            region_open: Duration::from_millis(4),
            ingest: Duration::from_millis(50),
            hash: Duration::from_millis(6),
            cas_put: Duration::from_millis(7),
            db_apply: Duration::from_millis(8),
            skipped_regions: 1,
            carried_chunks: 8,
            regions: vec![sekai_app::RegionTiming {
                path: PathBuf::from("region/r.0.0.mca"),
                bytes: 100,
                chunks: 32,
                open: Duration::from_millis(4),
                ingest: Duration::from_millis(5),
                hash: Duration::from_millis(6),
                cas: Duration::from_millis(7),
            }],
        };
        assert_eq!(
            json(&backup_payload(&backup_report, &backup_timings, false)),
            r#"{"snapshot":3,"chunks":40,"new_blobs":2,"tombstones":0,"skipped_regions":1,"carried_chunks":8}"#
        );
        assert_eq!(
            json(&backup_payload(&backup_report, &backup_timings, true)),
            r#"{"snapshot":3,"chunks":40,"new_blobs":2,"tombstones":0,"skipped_regions":1,"carried_chunks":8,"total_ms":100,"phases":{"discover_ms":1,"universe_load_ms":2,"fingerprint_ms":3,"region_open_ms":4,"ingest_ms":50,"hash_ms":6,"cas_put_ms":7,"db_apply_ms":8},"regions":[{"path":"region/r.0.0.mca","bytes":100,"chunks":32,"open_ms":4,"ingest_ms":5,"hash_ms":6,"cas_ms":7}]}"#
        );

        let rollback_report = RollbackReport {
            files_written: 2,
            files_deleted: 1,
            chunks_restored: 40,
        };
        let rollback_timings = RollbackTimings {
            total: Duration::from_millis(90),
            plan: Duration::from_millis(9),
            discover: Duration::from_millis(8),
            rollback_files: Duration::from_millis(70),
        };
        assert_eq!(
            json(&rollback_payload(
                &rollback_report,
                &rollback_timings,
                false
            )),
            r#"{"files_written":2,"files_deleted":1,"chunks_restored":40}"#
        );
        assert_eq!(
            json(&rollback_payload(&rollback_report, &rollback_timings, true)),
            r#"{"files_written":2,"files_deleted":1,"chunks_restored":40,"total_ms":90,"phases":{"plan_ms":9,"discover_ms":8,"rollback_files_ms":70}}"#
        );

        let gc_report = GcReport {
            candidates: 5,
            orphans: 4,
            removed: 4,
        };
        let gc_timings = GcTimings {
            total: Duration::from_millis(30),
            plan: Duration::from_millis(20),
            apply: Duration::from_millis(10),
        };
        assert_eq!(
            json(&gc_payload(&gc_report, &gc_timings, false)),
            r#"{"candidates":5,"orphans":4,"removed":4}"#
        );
        assert_eq!(
            json(&gc_payload(&gc_report, &gc_timings, true)),
            r#"{"candidates":5,"orphans":4,"removed":4,"total_ms":30,"phases":{"plan_ms":20,"apply_ms":10}}"#
        );
    }

    #[test]
    fn highlight_count_emphasizes_nonzero() {
        let style = Styler::enabled();
        assert_eq!(highlight_count(style, 0), "0");
        assert_eq!(highlight_count(style, 7), "\x1b[33m7\x1b[0m");
        assert_eq!(highlight_count(Styler::disabled(), 7), "7");
    }

    #[test]
    fn renders_error_with_chain() {
        let err = anyhow::anyhow!("torn tail at sector 1034").context("backup of /w failed");
        assert_eq!(
            render_error(&err, Styler::enabled()),
            "\x1b[1;31merror\x1b[0m: backup of /w failed\n\nCaused by:\n    torn tail at sector 1034"
        );
        assert_eq!(
            render_error(&err, Styler::disabled()),
            "error: backup of /w failed\n\nCaused by:\n    torn tail at sector 1034"
        );
    }
}
