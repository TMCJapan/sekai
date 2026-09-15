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

/// Chunk-level deduplicated snapshots for Minecraft region files.
#[derive(Debug, Parser)]
#[command(name = "sekai", version, about)]
struct Cli {
    /// Backup store directory (created when missing). Prefix with
    /// `sqlite://` explicitly if preferred.
    #[arg(long, global = true, default_value = "sekai-store")]
    store: String,

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
        #[arg(long, conflicts_with = "timing_json")]
        timing: bool,
        /// Print report and timings as flat JSON instead of human text.
        #[arg(long)]
        timing_json: bool,
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
        #[arg(long, conflicts_with = "timing_json")]
        timing: bool,
        /// Print report and timings as flat JSON instead of human text.
        #[arg(long)]
        timing_json: bool,
    },
    /// List recorded snapshots, oldest first.
    List,
    /// Compare chunk NBT AST between two snapshots or between world state and a snapshot.
    Diff(DiffArgs),
    /// Garbage collect unreferenced orphan blobs from the store.
    Gc {
        /// Inspect store and build plan without unlinking orphan blobs.
        #[arg(long)]
        dry_run: bool,
        /// Print a per-phase timing breakdown after the report.
        #[arg(long, conflicts_with = "timing_json")]
        timing: bool,
        /// Print report and timings as flat JSON instead of human text.
        #[arg(long)]
        timing_json: bool,
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
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Backup {
            world,
            timing,
            timing_json,
            with_diff,
            jobs,
        } => run_backup(&cli.store, &world, timing, timing_json, with_diff, jobs).await,
        Command::Rollback {
            world,
            snapshot,
            timing,
            timing_json,
        } => run_rollback(&cli.store, &world, snapshot, timing, timing_json).await,
        Command::List => run_list(&cli.store).await,
        Command::Diff(args) => run_diff(&cli.store, args).await,
        Command::Gc {
            dry_run,
            timing,
            timing_json,
        } => run_gc(&cli.store, dry_run, timing, timing_json).await,
        Command::Debug { debug } => match debug {
            DebugCommand::Scan { world, json } => run_debug_scan(&world, json),
        },
    }
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
    timing_json: bool,
    with_diff: bool,
    jobs: usize,
) -> anyhow::Result<()> {
    let (report, timings) = sekai_app::backup(world, store, options(with_diff, jobs), |_| {})
        .await
        .with_context(|| format!("backup of {} failed", world.display()))?;
    if timing_json {
        println!("{}", backup_json(&report, &timings));
        return Ok(());
    }
    println!(
        "snapshot {} recorded: {} chunks, {} new blobs, {} tombstones",
        report.snapshot.raw(),
        report.chunks,
        report.new_blobs,
        report.tombstones
    );
    if timing {
        print_timing_table(&timings);
    }
    Ok(())
}

async fn run_rollback(
    store: &str,
    world: &Path,
    snapshot: u64,
    timing: bool,
    timing_json: bool,
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
    if timing_json {
        println!("{}", rollback_json(&report, &timings));
        return Ok(());
    }
    println!(
        "snapshot {snapshot} restored: {} files rewritten, {} files deleted, {} chunks restored",
        report.files_written, report.files_deleted, report.chunks_restored
    );
    if timing {
        print_rollback_timing_table(&timings);
    }
    Ok(())
}

async fn run_list(store: &str) -> anyhow::Result<()> {
    for snapshot in sekai_app::list_snapshots(store).await? {
        println!(
            "{}\t{}",
            snapshot.id.raw(),
            format_time(snapshot.created_at_ms)
        );
    }
    Ok(())
}

async fn run_diff(store: &str, args: DiffArgs) -> anyhow::Result<()> {
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
        println!("{}", diff_json(&diffs));
        return Ok(());
    }

    if diffs.is_empty() {
        println!("No differences found for chunk ({}, {}).", args.cx, args.cz);
        return Ok(());
    }

    for entry in &diffs {
        let op = match &entry.change {
            NbtChange::Added(_) => "+",
            NbtChange::Removed(_) => "-",
            NbtChange::Modified { .. } => "~",
        };
        println!("{} {}", op, entry.path);
    }
    Ok(())
}

async fn run_gc(store: &str, dry_run: bool, timing: bool, timing_json: bool) -> anyhow::Result<()> {
    if dry_run {
        let plan = sekai_app::gc_plan(store)
            .await
            .with_context(|| format!("gc plan for {store} failed"))?;
        if timing_json {
            println!("{}", gc_plan_json(&plan));
            return Ok(());
        }
        println!(
            "gc plan created: {} orphan blobs (examined {})",
            plan.orphans.len(),
            plan.examined
        );
        return Ok(());
    }

    let (report, timings) = sekai_app::gc(store)
        .await
        .with_context(|| format!("gc for {store} failed"))?;
    if timing_json {
        println!("{}", gc_json(&report, &timings));
        return Ok(());
    }
    println!(
        "gc completed: {} removed, {} orphans, {} candidates",
        report.removed, report.orphans, report.candidates
    );
    if timing {
        print_gc_timing_table(&timings);
    }
    Ok(())
}

/// Unix millis to RFC 3339, falling back to the raw number when absurd.
fn format_time(created_at_ms: u64) -> String {
    let millis = i64::try_from(created_at_ms).unwrap_or(i64::MAX);
    chrono::DateTime::from_timestamp_millis(millis)
        .map_or_else(|| format!("{created_at_ms}ms"), |time| time.to_rfc3339())
}

/// Human-readable phase table for `backup --timing`.
fn print_timing_table(timings: &BackupTimings) {
    println!(
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
    );
    let mut slowest: Vec<&sekai_app::RegionTiming> = timings.regions.iter().collect();
    slowest.sort_by_key(|r| std::cmp::Reverse((r.open + r.ingest).as_micros()));
    for region in slowest.iter().take(5) {
        println!(
            "  {} chunks={} bytes={} open={}ms ingest={}ms (hash={}ms cas={}ms)",
            region.path.display(),
            region.chunks,
            region.bytes,
            region.open.as_millis(),
            region.ingest.as_millis(),
            region.hash.as_millis(),
            region.cas.as_millis(),
        );
    }
}

/// Human-readable phase table for `rollback --timing`.
fn print_rollback_timing_table(timings: &RollbackTimings) {
    println!(
        "timing total={}ms plan={}ms discover={}ms rollback_files={}ms",
        timings.total.as_millis(),
        timings.plan.as_millis(),
        timings.discover.as_millis(),
        timings.rollback_files.as_millis(),
    );
}

/// Human-readable phase table for `gc --timing`.
fn print_gc_timing_table(timings: &GcTimings) {
    println!(
        "timing total={}ms plan={}ms apply={}ms",
        timings.total.as_millis(),
        timings.plan.as_millis(),
        timings.apply.as_millis(),
    );
}

/// Flat JSON array for `diff --json`.
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
                format!("\"val\":\"{}\"", json_escape(&format!("{v:?}"))),
            ),
            NbtChange::Removed(v) => (
                "removed",
                format!("\"val\":\"{}\"", json_escape(&format!("{v:?}"))),
            ),
            NbtChange::Modified { old, new } => (
                "modified",
                format!(
                    "\"old\":\"{}\",\"new\":\"{}\"",
                    json_escape(&format!("{old:?}")),
                    json_escape(&format!("{new:?}"))
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

/// Flat JSON for `backup --timing-json`.
fn backup_json(report: &sekai_app::BackupReport, timings: &BackupTimings) -> String {
    use core::fmt::Write as _;
    let mut out = String::from("{");
    let _ = write!(
        out,
        "\"snapshot\":{},\"chunks\":{},\"new_blobs\":{},\"tombstones\":{},\"skipped_regions\":{},\"carried_chunks\":{},\"total_ms\":{}",
        report.snapshot.raw(),
        report.chunks,
        report.new_blobs,
        report.tombstones,
        report.skipped_regions,
        report.carried_chunks,
        timings.total.as_millis(),
    );
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

/// Flat JSON for `rollback --timing-json`.
fn rollback_json(report: &RollbackReport, timings: &RollbackTimings) -> String {
    use core::fmt::Write as _;
    let mut out = String::from("{");
    let _ = write!(
        out,
        "\"files_written\":{},\"files_deleted\":{},\"chunks_restored\":{},\"total_ms\":{}",
        report.files_written,
        report.files_deleted,
        report.chunks_restored,
        timings.total.as_millis(),
    );
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

/// Flat JSON for `gc --timing-json`.
fn gc_json(report: &GcReport, timings: &GcTimings) -> String {
    use core::fmt::Write as _;
    let mut out = String::from("{");
    let _ = write!(
        out,
        "\"candidates\":{},\"orphans\":{},\"removed\":{},\"total_ms\":{}",
        report.candidates,
        report.orphans,
        report.removed,
        timings.total.as_millis(),
    );
    let _ = write!(
        out,
        ",\"phases\":{{\"plan_ms\":{},\"apply_ms\":{}}}",
        timings.plan.as_millis(),
        timings.apply.as_millis(),
    );
    out.push('}');
    out
}

/// Flat JSON for `gc --dry-run --timing-json`.
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
        println!("{}", scan_json(&entries));
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
            })
        ));
    }

    #[test]
    fn parses_timing_and_debug_scan() {
        let cli = Cli::try_parse_from(["sekai", "backup", "--timing", "world"])
            .expect("backup --timing parses");
        assert!(matches!(cli.command, Command::Backup { timing: true, .. }));

        let cli = Cli::try_parse_from(["sekai", "backup", "--timing-json", "world"])
            .expect("backup --timing-json parses");
        assert!(matches!(
            cli.command,
            Command::Backup {
                timing_json: true,
                ..
            }
        ));

        // The two renderings are mutually exclusive.
        assert!(
            Cli::try_parse_from(["sekai", "backup", "--timing", "--timing-json", "world"]).is_err()
        );

        let cli = Cli::try_parse_from(["sekai", "rollback", "--timing", "w", "3"])
            .expect("rollback --timing parses");
        assert!(matches!(
            cli.command,
            Command::Rollback { timing: true, .. }
        ));

        let cli = Cli::try_parse_from(["sekai", "rollback", "--timing-json", "w", "3"])
            .expect("rollback --timing-json parses");
        assert!(matches!(
            cli.command,
            Command::Rollback {
                timing_json: true,
                ..
            }
        ));

        assert!(
            Cli::try_parse_from(["sekai", "rollback", "--timing", "--timing-json", "w", "3"])
                .is_err()
        );

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
                timing_json: false
            }
        ));

        let cli = Cli::try_parse_from(["sekai", "gc", "--dry-run", "--timing-json"])
            .expect("gc --dry-run --timing-json parses");
        assert!(matches!(
            cli.command,
            Command::Gc {
                dry_run: true,
                timing_json: true,
                ..
            }
        ));

        assert!(Cli::try_parse_from(["sekai", "gc", "--timing", "--timing-json"]).is_err());
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
}
