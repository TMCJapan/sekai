//! Thin `sekai` binary over `sekai-app`.
//!
//! Argument parsing and output formatting only. Quiesce the server before
//! snapshotting (e.g. `save-off`, `save-all`, then `save-on` afterwards);
//! orchestration belongs to the caller, never to this tool.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use sekai_app::{
    BackupOptions, BackupTimings, ChunkCoord, Dimension, GcPlan, GcReport, GcTimings, NbtChange,
    RegionKind, RollbackReport, RollbackTimings, SnapshotId,
};

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
    /// Chunk X coordinate.
    #[arg(long, allow_hyphen_values = true)]
    cx: i32,
    /// Chunk Z coordinate.
    #[arg(long, allow_hyphen_values = true)]
    cz: i32,
    /// Dimension string.
    #[arg(long, default_value = "overworld")]
    dim: Dimension,
    /// Region kind string.
    #[arg(long, default_value = "region")]
    kind: RegionKind,
    /// Emit diff array as JSON instead of human text.
    #[arg(long)]
    json: bool,
    /// Show concrete old/new values in human output (SNBT format).
    #[arg(long)]
    show_values: bool,
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
        } => run_backup(&cli.store, &world, timing, json, with_diff, jobs, style).await,
        Command::Rollback {
            world,
            snapshot,
            timing,
            json,
        } => run_rollback(&cli.store, &world, snapshot, timing, json, style).await,
        Command::List { json } => run_list(&cli.store, json, style).await,
        Command::Diff(args) => run_diff(&cli.store, args, style).await,
        Command::Gc {
            dry_run,
            timing,
            json,
        } => run_gc(&cli.store, dry_run, timing, json, style).await,
        Command::Debug { debug } => match debug {
            DebugCommand::Scan { world, json } => run_debug_scan(&world, json),
        },
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            if as_json {
                println!("{}", envelope_err(command_name, &err));
            } else {
                eprintln!("{}", render_error(&err, Styler::new_stderr(cli.color)));
            }
            std::process::ExitCode::FAILURE
        }
    }
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
fn envelope_ok(command: &str, result_json: &str) -> String {
    format!("{{\"command\":\"{command}\",\"status\":\"ok\",\"result\":{result_json}}}")
}

/// JSON error object: stdout stays exactly one JSON document.
/// Callers must still check the exit code; `error` is only present here.
fn envelope_err(command: &str, err: &anyhow::Error) -> String {
    format!(
        "{{\"command\":\"{command}\",\"status\":\"error\",\"error\":\"{}\"}}",
        json_escape(&format!("{err:?}"))
    )
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
    timing: bool,
    json: bool,
    with_diff: bool,
    jobs: usize,
    style: Styler,
) -> anyhow::Result<()> {
    let (report, timings) = sekai_app::backup(world, store, options(with_diff, jobs), |_| {})
        .await
        .with_context(|| format!("backup of {} failed", world.display()))?;
    if json {
        println!(
            "{}",
            envelope_ok("backup", &backup_json(&report, &timings, timing))
        );
        return Ok(());
    }
    println!(
        "snapshot {} recorded: {} chunks, {} new blobs, {} tombstones",
        style.bold(&report.snapshot.raw().to_string()),
        report.chunks,
        report.new_blobs,
        report.tombstones
    );
    if timing {
        print_timing_table(&timings, style);
    }
    Ok(())
}

async fn run_rollback(
    store: &str,
    world: &Path,
    snapshot: u64,
    timing: bool,
    json: bool,
    style: Styler,
) -> anyhow::Result<()> {
    let id = SnapshotId(snapshot);
    let (report, timings) = sekai_app::rollback(world, store, id)
        .await
        .with_context(|| {
            format!(
                "rollback of {} to snapshot {snapshot} failed",
                world.display()
            )
        })?;
    if json {
        println!(
            "{}",
            envelope_ok("rollback", &rollback_json(&report, &timings, timing))
        );
        return Ok(());
    }
    println!(
        "snapshot {} restored: {} files rewritten, {} files deleted, {} chunks restored",
        style.bold(&snapshot.to_string()),
        report.files_written,
        report.files_deleted,
        report.chunks_restored
    );
    if timing {
        print_rollback_timing_table(&timings, style);
    }
    Ok(())
}

async fn run_list(store: &str, json: bool, style: Styler) -> anyhow::Result<()> {
    let snapshots = sekai_app::list_snapshots(store).await?;
    if json {
        println!("{}", envelope_ok("list", &list_json(&snapshots)));
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

/// Truncate a rendered value to [`MAX_VALUE_CHARS`] chars (char boundary),
/// appending an ellipsis. Paths are never truncated.
fn truncate_value(rendered: &str) -> String {
    const MAX_VALUE_CHARS: usize = 500;
    if rendered.chars().count() <= MAX_VALUE_CHARS {
        return rendered.to_owned();
    }
    let truncated: String = rendered.chars().take(MAX_VALUE_CHARS).collect();
    format!("{truncated}...")
}

async fn run_diff(store: &str, args: DiffArgs, style: Styler) -> anyhow::Result<()> {
    let coord = ChunkCoord::new(args.dim, args.kind, args.cx, args.cz);

    let diffs = if let Some(world) = &args.world {
        let snapshot_id = args.old_snapshot.or(args.new_snapshot).map(SnapshotId);
        sekai_app::diff_world_chunk(world, store, snapshot_id, &coord, None)
            .await
            .with_context(|| {
                format!(
                    "failed to compute chunk diff for ({}, {}) between world state and snapshot",
                    args.cx, args.cz
                )
            })?
    } else {
        let (old_id, new_id) = match (args.old_snapshot, args.new_snapshot) {
            (Some(old), Some(new)) => (SnapshotId(old), SnapshotId(new)),
            (Some(old), None) => {
                let latest = sekai_app::latest_snapshot_id(store).await?;
                (SnapshotId(old), latest)
            }
            (None, None) => {
                let snapshots = sekai_app::list_snapshots(store).await?;
                if snapshots.len() < 2 {
                    anyhow::bail!(
                        "at least 2 snapshots are required when snapshot IDs are omitted"
                    );
                }
                let old = snapshots[snapshots.len() - 2].id;
                let new = snapshots[snapshots.len() - 1].id;
                (old, new)
            }
            (None, Some(new)) => {
                let snapshots = sekai_app::list_snapshots(store).await?;
                if snapshots.len() < 2 {
                    anyhow::bail!(
                        "at least 2 snapshots are required when snapshot IDs are omitted"
                    );
                }
                let old = snapshots[snapshots.len() - 2].id;
                (old, SnapshotId(new))
            }
        };

        sekai_app::diff_chunk(store, old_id, new_id, &coord, None)
            .await
            .with_context(|| {
                format!(
                    "failed to compute chunk diff for ({}, {}) between snapshot {} and {}",
                    args.cx,
                    args.cz,
                    old_id.raw(),
                    new_id.raw()
                )
            })?
    };

    if args.json {
        println!("{}", envelope_ok("diff", &diff_json(&diffs)));
        return Ok(());
    }

    println!(
        "{}",
        render_diff_human(args.cx, args.cz, &diffs, args.show_values, style)
    );
    Ok(())
}

async fn run_gc(
    store: &str,
    dry_run: bool,
    timing: bool,
    json: bool,
    style: Styler,
) -> anyhow::Result<()> {
    if dry_run {
        let plan = sekai_app::gc_plan(store)
            .await
            .with_context(|| format!("gc plan for {store} failed"))?;
        if json {
            println!("{}", envelope_ok("gc", &gc_plan_json(&plan)));
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
    if json {
        println!("{}", envelope_ok("gc", &gc_json(&report, &timings, timing)));
        return Ok(());
    }
    println!(
        "gc completed: {} removed, {} orphans, {} candidates",
        highlight_count(style, report.removed),
        report.orphans,
        report.candidates
    );
    if timing {
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

/// Flat JSON array for `diff --json`. Values are SNBT strings.
fn diff_json(diffs: &[sekai_app::NbtDiffEntry]) -> String {
    use core::fmt::Write as _;
    let mut out = String::from("[");
    for (index, entry) in diffs.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let (change_type, details) = match &entry.change {
            NbtChange::Added(v) => (
                "added",
                format!("\"val\":\"{}\"", json_escape(&format!("{v}"))),
            ),
            NbtChange::Removed(v) => (
                "removed",
                format!("\"val\":\"{}\"", json_escape(&format!("{v}"))),
            ),
            NbtChange::Modified { old, new } => (
                "modified",
                format!(
                    "\"old\":\"{}\",\"new\":\"{}\"",
                    json_escape(&format!("{old}")),
                    json_escape(&format!("{new}"))
                ),
            ),
        };
        let _ = write!(
            out,
            "{{\"path\":\"{}\",\"type\":\"{change_type}\",{details}}}",
            json_escape(&entry.path)
        );
    }
    out.push(']');
    out
}

/// JSON report for `backup --json`. Without `--timing` this is the bare
/// result; with `--timing` the `total_ms`/`phases`/`regions` block is
/// appended, mirroring the human `--timing` table plus the full region list.
fn backup_json(report: &sekai_app::BackupReport, timings: &BackupTimings, timing: bool) -> String {
    use core::fmt::Write as _;
    let mut out = String::from("{");
    let _ = write!(
        out,
        "\"snapshot\":{},\"chunks\":{},\"new_blobs\":{},\"tombstones\":{},\"skipped_regions\":{},\"carried_chunks\":{}",
        report.snapshot.raw(),
        report.chunks,
        report.new_blobs,
        report.tombstones,
        report.skipped_regions,
        report.carried_chunks,
    );
    if !timing {
        out.push('}');
        return out;
    }
    let _ = write!(out, ",\"total_ms\":{}", timings.total.as_millis());
    let _ = write!(
        out,
        ",\"phases\":{{\"discover_ms\":{},\"universe_load_ms\":{},\"fingerprint_ms\":{},\"region_open_ms\":{},\"ingest_ms\":{},\"hash_ms\":{},\"cas_put_ms\":{},\"db_apply_ms\":{}}}",
        timings.discover.as_millis(),
        timings.universe_load.as_millis(),
        timings.fingerprint.as_millis(),
        timings.region_open.as_millis(),
        timings.ingest.as_millis(),
        timings.hash.as_millis(),
        timings.cas_put.as_millis(),
        timings.db_apply.as_millis(),
    );
    out.push_str(",\"regions\":[");
    for (index, region) in timings.regions.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let _ = write!(
            out,
            "{{\"path\":\"{}\",\"bytes\":{},\"chunks\":{},\"open_ms\":{},\"ingest_ms\":{},\"hash_ms\":{},\"cas_ms\":{}}}",
            json_escape(&region.path.to_string_lossy()),
            region.bytes,
            region.chunks,
            region.open.as_millis(),
            region.ingest.as_millis(),
            region.hash.as_millis(),
            region.cas.as_millis(),
        );
    }
    out.push_str("]}");
    out
}

/// JSON report for `rollback --json`; the timing block needs `--timing`.
fn rollback_json(report: &RollbackReport, timings: &RollbackTimings, timing: bool) -> String {
    use core::fmt::Write as _;
    let mut out = String::from("{");
    let _ = write!(
        out,
        "\"files_written\":{},\"files_deleted\":{},\"chunks_restored\":{}",
        report.files_written, report.files_deleted, report.chunks_restored,
    );
    if !timing {
        out.push('}');
        return out;
    }
    let _ = write!(out, ",\"total_ms\":{}", timings.total.as_millis());
    let _ = write!(
        out,
        ",\"phases\":{{\"plan_ms\":{},\"discover_ms\":{},\"rollback_files_ms\":{}}}",
        timings.plan.as_millis(),
        timings.discover.as_millis(),
        timings.rollback_files.as_millis(),
    );
    out.push('}');
    out
}

/// JSON report for `gc --json`; the timing block needs `--timing`.
/// The dry-run plan (`gc --dry-run --json`) has its own shape, see
/// [`gc_plan_json`], since planning produces no phase timings.
fn gc_json(report: &GcReport, timings: &GcTimings, timing: bool) -> String {
    use core::fmt::Write as _;
    let mut out = String::from("{");
    let _ = write!(
        out,
        "\"candidates\":{},\"orphans\":{},\"removed\":{}",
        report.candidates, report.orphans, report.removed,
    );
    if !timing {
        out.push('}');
        return out;
    }
    let _ = write!(out, ",\"total_ms\":{}", timings.total.as_millis());
    let _ = write!(
        out,
        ",\"phases\":{{\"plan_ms\":{},\"apply_ms\":{}}}",
        timings.plan.as_millis(),
        timings.apply.as_millis(),
    );
    out.push('}');
    out
}

/// JSON plan for `gc --dry-run --json`. No phase timings exist for a plan,
/// so this shape never carries a timing block.
fn gc_plan_json(plan: &GcPlan) -> String {
    use core::fmt::Write as _;
    let mut out = String::from("{");
    let _ = write!(
        out,
        "\"orphans\":{},\"examined\":{}",
        plan.orphans.len(),
        plan.examined
    );
    out.push('}');
    out
}

fn run_debug_scan(world: &Path, json: bool) -> anyhow::Result<()> {
    let entries =
        sekai_app::scan(world).with_context(|| format!("scan of {} failed", world.display()))?;
    if json {
        println!("{}", envelope_ok("scan", &scan_json(&entries)));
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
fn list_json(snapshots: &[sekai_app::Snapshot]) -> String {
    use core::fmt::Write as _;
    let mut out = String::from("[");
    for (index, snapshot) in snapshots.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let _ = write!(
            out,
            "{{\"id\":{},\"created_at_ms\":{}}}",
            snapshot.id.raw(),
            snapshot.created_at_ms,
        );
    }
    out.push(']');
    out
}

/// Flat JSON array for `debug scan --json`.
fn scan_json(entries: &[sekai_app::RegionScanEntry]) -> String {
    use core::fmt::Write as _;
    let mut out = String::from("[");
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let mtime = entry
            .mtime_ms
            .map_or_else(|| "null".to_owned(), |ms| ms.to_string());
        let _ = write!(
            out,
            "{{\"path\":\"{}\",\"dim\":{},\"kind\":{},\"region_x\":{},\"region_z\":{},\"size\":{},\"mtime_ms\":{mtime},\"chunks\":{},\"header_hash\":\"{}\"}}",
            json_escape(&entry.path.to_string_lossy()),
            entry.dim.raw(),
            entry.kind.raw(),
            entry.region_x,
            entry.region_z,
            entry.file_bytes,
            entry.chunks,
            entry.header_hash,
        );
    }
    out.push(']');
    out
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

/// Minimal JSON string escaper for paths (quote, backslash, controls).
fn json_escape(text: &str) -> String {
    use core::fmt::Write as _;
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::time::Duration;
    use std::path::PathBuf;

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
        let cli = Cli::try_parse_from(["sekai", "diff", "1", "2", "--cx", "10", "--cz", "-5"])
            .expect("diff parses");
        assert!(matches!(
            cli.command,
            Command::Diff(DiffArgs {
                world: None,
                old_snapshot: Some(1),
                new_snapshot: Some(2),
                cx: 10,
                cz: -5,
                dim: Dimension::OVERWORLD,
                kind: RegionKind::REGION,
                json: false,
                show_values: false,
            })
        ));

        let cli = Cli::try_parse_from([
            "sekai", "diff", "--world", "world", "--cx", "10", "--cz", "-5",
        ])
        .expect("diff with world parses");
        assert!(matches!(
            cli.command,
            Command::Diff(DiffArgs {
                world: Some(_),
                old_snapshot: None,
                new_snapshot: None,
                cx: 10,
                cz: -5,
                dim: Dimension::OVERWORLD,
                kind: RegionKind::REGION,
                json: false,
                show_values: false,
            })
        ));

        let cli = Cli::try_parse_from([
            "sekai",
            "diff",
            "1",
            "2",
            "--cx",
            "0",
            "--cz",
            "0",
            "--show-values",
        ])
        .expect("diff --show-values parses");
        assert!(matches!(
            cli.command,
            Command::Diff(DiffArgs {
                show_values: true,
                ..
            })
        ));
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
    fn escapes_json_paths() {
        assert_eq!(json_escape("a\"b\\c"), "a\\\"b\\\\c");
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
        assert!(diff_json(&diffs).contains(&"x".repeat(600)));
    }

    #[test]
    fn renders_diff_json_with_snbt_values() {
        assert_eq!(
            diff_json(&sample_diffs()),
            r#"[{"path":"Status","type":"modified","old":"\"minecraft:full\"","new":"\"minecraft:empty\""},{"path":"xPos","type":"added","val":"3"},{"path":"old_tag","type":"removed","val":"1b"}]"#
        );
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

        let cli = Cli::try_parse_from(["sekai", "diff", "--cx", "0", "--cz", "0"]).expect("parses");
        assert!(!output_json(&cli.command));

        let cli = Cli::try_parse_from(["sekai", "diff", "--cx", "0", "--cz", "0", "--json"])
            .expect("parses");
        assert!(output_json(&cli.command));

        assert!(Cli::try_parse_from(["sekai", "backup", "--timing-json", "w"]).is_err());
    }

    #[test]
    fn renders_json_envelopes() {
        assert_eq!(
            envelope_ok("list", "[1,2]"),
            r#"{"command":"list","status":"ok","result":[1,2]}"#
        );
        let err = anyhow::anyhow!("root cause").context("backup of /w failed");
        assert_eq!(
            envelope_err("backup", &err),
            r#"{"command":"backup","status":"error","error":"backup of /w failed\n\nCaused by:\n    root cause"}"#
        );
    }

    #[test]
    fn renders_list_json() {
        assert_eq!(list_json(&[]), "[]");
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
            list_json(&snapshots),
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
            backup_json(&backup_report, &backup_timings, false),
            r#"{"snapshot":3,"chunks":40,"new_blobs":2,"tombstones":0,"skipped_regions":1,"carried_chunks":8}"#
        );
        assert_eq!(
            backup_json(&backup_report, &backup_timings, true),
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
            rollback_json(&rollback_report, &rollback_timings, false),
            r#"{"files_written":2,"files_deleted":1,"chunks_restored":40}"#
        );
        assert_eq!(
            rollback_json(&rollback_report, &rollback_timings, true),
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
            gc_json(&gc_report, &gc_timings, false),
            r#"{"candidates":5,"orphans":4,"removed":4}"#
        );
        assert_eq!(
            gc_json(&gc_report, &gc_timings, true),
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
