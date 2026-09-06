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
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Backup { world } => run_backup(&cli.store, &world),
        Command::Rollback { world, snapshot } => run_rollback(&cli.store, &world, snapshot),
        Command::List => run_list(&cli.store),
    }
}

fn open_store(store: &Path) -> anyhow::Result<sekai_engine::Store> {
    sekai_engine::Store::open(store)
        .with_context(|| format!("cannot open store at {}", store.display()))
}

fn run_backup(store_dir: &Path, world: &Path) -> anyhow::Result<()> {
    let mut store = open_store(store_dir)?;
    let report = sekai_engine::backup(world, &mut store)
        .with_context(|| format!("backup of {} failed", world.display()))?;
    println!(
        "snapshot {} recorded: {} chunks, {} new blobs, {} tombstones",
        report.snapshot.raw(),
        report.chunks,
        report.new_blobs,
        report.tombstones
    );
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

        run_backup(&store, &root.join("world")).expect("backup works");
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
