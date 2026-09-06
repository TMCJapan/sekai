#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::{Parser, Subcommand};

/// Chunk-level deduplicated snapshots for Minecraft region files.
///
/// Quiesce the server before snapshotting (e.g. `save-off`, `save-all`,
/// then `save-on` afterwards). `sekai` never touches the server process;
/// that orchestration belongs to the caller.
#[derive(Debug, Parser)]
#[command(name = "sekai", version, about)]
struct Cli {
    /// Backup store directory (created when missing).
    #[arg(long, global = true, default_value = "sekai-store")]
    store: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Record the current world state as a new snapshot.
    Backup {
        /// World directory (the one containing `region/`, `DIM-1/`, or `dimensions/`).
        world: PathBuf,
        /// Print a per-phase timing breakdown after the report.
        #[arg(long, conflicts_with = "timing_json")]
        timing: bool,
        /// Print report and timings as flat JSON instead of human text.
        #[arg(long)]
        timing_json: bool,
    },
    /// Rebuild the world from a snapshot, overwriting region files.
    Rollback {
        /// World directory to rebuild in place.
        world: PathBuf,
        /// Snapshot ID to restore (see `list`).
        snapshot: u64,
    },
    /// List recorded snapshots, oldest first.
    List,
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

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Backup {
            world,
            timing,
            timing_json,
        } => run_backup(&cli.store, &world, timing, timing_json),
        Command::Rollback { world, snapshot } => run_rollback(&cli.store, &world, snapshot),
        Command::List => run_list(&cli.store),
        Command::Debug { debug } => match debug {
            DebugCommand::Scan { world, json } => run_debug_scan(&world, json),
        },
    }
}

fn open_store(store: &Path) -> anyhow::Result<sekai_engine::Store> {
    sekai_engine::Store::open(store)
        .with_context(|| format!("cannot open store at {}", store.display()))
}

fn run_backup(
    store_dir: &Path,
    world: &Path,
    timing: bool,
    timing_json: bool,
) -> anyhow::Result<()> {
    let mut store = open_store(store_dir)?;
    let (report, timings) = sekai_engine::backup_with_metrics(world, &mut store)
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

fn run_rollback(store_dir: &Path, world: &Path, snapshot: u64) -> anyhow::Result<()> {
    let mut store = open_store(store_dir)?;
    let id = sekai_core::SnapshotId(snapshot);
    let report = sekai_engine::rollback(world, &mut store, id).with_context(|| {
        format!(
            "rollback of {} to snapshot {snapshot} failed",
            world.display()
        )
    })?;
    println!(
        "snapshot {snapshot} restored: {} files rewritten, {} files deleted, {} chunks restored",
        report.files_written, report.files_deleted, report.chunks_restored
    );
    Ok(())
}

fn run_list(store_dir: &Path) -> anyhow::Result<()> {
    let store = open_store(store_dir)?;
    for snapshot in sekai_engine::list_snapshots(&store)? {
        println!(
            "{}\t{}",
            snapshot.id.raw(),
            format_time(snapshot.created_at_ms)
        );
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
fn print_timing_table(timings: &sekai_engine::BackupTimings) {
    println!(
        "timing total={}ms discover={}ms universe={}ms open={}ms ingest={}ms (hash={}ms cas={}ms) db={}ms files={} cas_checked={}",
        timings.total.as_millis(),
        timings.discover.as_millis(),
        timings.universe_load.as_millis(),
        timings.region_open.as_millis(),
        timings.ingest.as_millis(),
        timings.hash.as_millis(),
        timings.cas_put.as_millis(),
        timings.db_apply.as_millis(),
        timings.regions.len(),
        timings.cas_checked,
    );
    let mut slowest: Vec<&sekai_engine::RegionTiming> = timings.regions.iter().collect();
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

/// Flat JSON for `backup --timing-json` (hand-rolled to avoid a serde
/// dependency for one flag).
fn backup_json(
    report: &sekai_engine::BackupReport,
    timings: &sekai_engine::BackupTimings,
) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("{");
    let _ = write!(
        out,
        "\"snapshot\":{},\"chunks\":{},\"new_blobs\":{},\"tombstones\":{},\"total_ms\":{}",
        report.snapshot.raw(),
        report.chunks,
        report.new_blobs,
        report.tombstones,
        timings.total.as_millis(),
    );
    let _ = write!(
        out,
        ",\"phases\":{{\"discover_ms\":{},\"universe_load_ms\":{},\"region_open_ms\":{},\"ingest_ms\":{},\"hash_ms\":{},\"cas_put_ms\":{},\"db_apply_ms\":{}}}",
        timings.discover.as_millis(),
        timings.universe_load.as_millis(),
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

fn run_debug_scan(world: &Path, json: bool) -> anyhow::Result<()> {
    let entries = sekai_engine::scan_world(world)
        .with_context(|| format!("scan of {} failed", world.display()))?;
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
                .map_or_else(|| "n/a".to_string(), |ms| ms.to_string()),
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

/// Flat JSON array for `debug scan --json` (hand-rolled to avoid a serde
/// dependency for one flag).
fn scan_json(entries: &[sekai_engine::RegionScanEntry]) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("[");
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let mtime = entry
            .mtime_ms
            .map_or_else(|| "null".to_string(), |ms| ms.to_string());
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
fn kind_name(kind: sekai_core::RegionKind) -> &'static str {
    if kind == sekai_core::RegionKind::REGION {
        "region"
    } else if kind == sekai_core::RegionKind::ENTITIES {
        "entities"
    } else if kind == sekai_core::RegionKind::POI {
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
        assert_eq!(cli.store, PathBuf::from("sekai-store"));

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

        let cli =
            Cli::try_parse_from(["sekai", "debug", "scan", "world"]).expect("debug scan parses");
        assert!(matches!(cli.command, Command::Debug { .. }));
    }

    #[test]
    fn formats_times() {
        assert_eq!(format_time(0), "1970-01-01T00:00:00+00:00");
        // Absurd values fall back instead of panicking.
        assert_eq!(format_time(u64::MAX), format!("{}ms", u64::MAX));
    }

    #[test]
    fn backup_and_list_round_trip() {
        let root: PathBuf =
            std::env::temp_dir().join(format!("sekai-cli-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("world").join("region")).expect("setup dirs");
        let region = root.join("world").join("region").join("r.0.0.mca");
        write_region(&region);
        let store = root.join("store");

        run_backup(&store, &root.join("world"), false, false).expect("backup works");
        let opened = open_store(&store).expect("store opens");
        let snapshots = sekai_engine::list_snapshots(&opened).expect("list works");
        assert_eq!(snapshots.len(), 1);
        run_rollback(&store, &root.join("world"), snapshots[0].id.raw()).expect("rollback works");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Minimal one-chunk region for the smoke test.
    fn write_region(path: &Path) {
        use sekai_core::{ChunkCoord, Dimension, RegionKind, RegionWriter as _};
        let mut writer =
            sekai_mca::RegionFileWriter::create(path, Dimension::OVERWORLD, RegionKind::REGION, 0)
                .expect("writer creates");
        writer
            .stage_chunk(
                &ChunkCoord::new(Dimension::OVERWORLD, RegionKind::REGION, 0, 0),
                &[2, 1, 2, 3],
            )
            .expect("stage works");
        writer.commit().expect("commit works");
    }
}
