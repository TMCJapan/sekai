//! Diff subcommand execution, DTOs, and coordination.

use anyhow::Context as _;
use sekai_app::{ChunkCoord, ChunkDiff, RegionKey, SnapshotId};

use super::diff_render::{
    TimedDiffs, diff_entry_payload, diff_group_payload, print_diff_timing_table,
    render_diff_grouped, render_diff_human,
};
use crate::cli::{DiffArgs, Selection};
use crate::envelope::envelope_ok;
use crate::style::Styler;

pub async fn run(store: &str, args: &DiffArgs, style: Styler) -> anyhow::Result<()> {
    let selection = &args.selection;
    let explicit = selection.chunks();

    let (diffs, timings) = if let Some(world) = &args.world {
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

    if diffs.len() == 1 {
        let diff = &diffs[0];
        if args.output.json {
            if args.output.timing {
                println!(
                    "{}",
                    envelope_ok(
                        "diff",
                        &TimedDiffs {
                            diffs: diff_entry_payload(&diff.entries),
                            timing: Some((&timings).into()),
                        }
                    )?
                );
            } else {
                println!(
                    "{}",
                    envelope_ok("diff", &diff_entry_payload(&diff.entries))?
                );
            }
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
        if args.output.timing {
            print_diff_timing_table(&timings, diffs.len(), style);
        }
        return Ok(());
    }

    let nonempty: Vec<&ChunkDiff> = diffs
        .iter()
        .filter(|diff| !diff.entries.is_empty())
        .collect();
    if args.output.json {
        if args.output.timing {
            println!(
                "{}",
                envelope_ok(
                    "diff",
                    &TimedDiffs {
                        diffs: diff_group_payload(&nonempty),
                        timing: Some((&timings).into()),
                    }
                )?
            );
        } else {
            println!("{}", envelope_ok("diff", &diff_group_payload(&nonempty))?);
        }
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
    if args.output.timing {
        print_diff_timing_table(&timings, diffs.len(), style);
    }
    Ok(())
}

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
        (None, new) => {
            let snapshots = sekai_app::list_snapshots(store).await?;
            if snapshots.len() < 2 {
                anyhow::bail!("at least 2 snapshots are required when snapshot IDs are omitted");
            }
            let old = snapshots[snapshots.len() - 2].id;
            let new = new.map_or(snapshots[snapshots.len() - 1].id, SnapshotId);
            Ok((old, new))
        }
    }
}

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
