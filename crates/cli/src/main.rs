//! Thin `sekai` binary over `sekai-app`.
//!
//! Argument parsing and output formatting only. Quiesce the server before
//! snapshotting (e.g. `save-off`, `save-all`, then `save-on` afterwards);
//! orchestration belongs to the caller, never to this tool.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use sekai_app::{
    BackupOptions, BackupTimings, GcPlan, GcReport, GcTimings, RollbackReport, RollbackTimings,
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
    let id = sekai_app::SnapshotId(snapshot);
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
///
/// Totals first (disjoint phases sum to roughly the wall total; `hash` and
/// `cas` are the per-chunk split inside `ingest`), then the five slowest
/// regions by `open + ingest` so a few changed regions stand out.
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

/// Flat JSON for `backup --timing-json` (hand-rolled to avoid a serde
/// dependency for one flag).
fn backup_json(report: &sekai_app::BackupReport, timings: &BackupTimings) -> String {
    use std::fmt::Write as _;
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
    use std::fmt::Write as _;
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
    use std::fmt::Write as _;
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
    use std::fmt::Write as _;
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
            entry.header_hash.get(..12).unwrap_or(&entry.header_hash),
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

/// Flat JSON array for `debug scan --json` (hand-rolled, see `backup_json`).
fn scan_json(entries: &[sekai_app::RegionScanEntry]) -> String {
    use std::fmt::Write as _;
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
fn kind_name(kind: sekai_app::RegionKind) -> &'static str {
    if kind == sekai_app::RegionKind::REGION {
        "region"
    } else if kind == sekai_app::RegionKind::ENTITIES {
        "entities"
    } else if kind == sekai_app::RegionKind::POI {
        "poi"
    } else {
        "unknown"
    }
}

/// Minimal JSON string escaper for paths (quote, backslash, controls).
fn json_escape(text: &str) -> String {
    use std::fmt::Write as _;
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
