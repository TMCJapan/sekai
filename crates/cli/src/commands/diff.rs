//! Diff subcommand execution, DTOs, and coordination.

use anyhow::Context as _;
use sekai_app::{ChunkCoord, ChunkDiff, Scope, SnapshotId};

use super::diff_render::{
    TimedDiffs, diff_entry_payload, diff_group_payload, print_diff_timing_table,
    render_diff_grouped, render_diff_human,
};
use super::progress::{finish_progress, progress_bar, report_progress};
use crate::cli::DiffArgs;
use crate::envelope::envelope_ok;
use crate::style::Styler;

pub async fn run(store: &str, args: &DiffArgs, style: Styler) -> anyhow::Result<()> {
    let selection = &args.selection;
    let scope = selection.owned_scope();
    let mut coords = selection.explicit_chunks();
    let snapshot_pair = if args.world.is_none() {
        Some(
            resolve_snapshot_pair(store, args.old_snapshot.clone(), args.new_snapshot.clone())
                .await?,
        )
    } else {
        None
    };
    // Broad areas (whole dimensions, rectangles) and empty selections
    // resolve through enumeration; explicit chunks join the union.
    if selection.has_broad_areas() || coords.is_empty() {
        let mut enumerated = if let Some(world) = &args.world {
            sekai_app::world_chunk_coords(world)?
        } else if let Some((old_id, new_id)) = snapshot_pair {
            let mut union = sekai_app::snapshot_chunk_coords(store, old_id).await?;
            union.extend(sekai_app::snapshot_chunk_coords(store, new_id).await?);
            union
        } else {
            anyhow::bail!("internal error: snapshot pair missing for snapshot diff");
        };
        coords.append(&mut enumerated);
    }
    let coords = scoped_coords(coords, &scope);
    let bar = progress_bar(args.progress);

    let (diffs, timings) = if let Some(world) = &args.world {
        let snapshot_ref = args
            .old_snapshot
            .as_deref()
            .or(args.new_snapshot.as_deref());
        let snapshot_id = if let Some(raw) = snapshot_ref {
            Some(
                sekai_app::resolve_snapshot_ref(store, raw)
                    .await
                    .with_context(|| format!("snapshot {raw:?} failed to resolve"))?,
            )
        } else {
            None
        };
        sekai_app::diff_world_chunks(world, store, snapshot_id, &coords, None, |update| {
            report_progress(
                bar.as_ref(),
                update.chunks_done,
                update.chunks_total,
                "chunks",
            );
        })
        .await
        .with_context(|| "failed to compute chunk diffs between world state and snapshot")?
    } else {
        let Some((old_id, new_id)) = snapshot_pair else {
            anyhow::bail!("internal error: snapshot pair missing for snapshot diff");
        };
        sekai_app::diff_chunks(store, old_id, new_id, &coords, None, |update| {
            report_progress(
                bar.as_ref(),
                update.chunks_done,
                update.chunks_total,
                "chunks",
            );
        })
        .await
        .with_context(|| {
            format!(
                "failed to compute chunk diffs between snapshot {} and {}",
                old_id.raw(),
                new_id.raw()
            )
        })?
    };
    finish_progress(bar.as_ref());

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
    old_snapshot: Option<String>,
    new_snapshot: Option<String>,
) -> anyhow::Result<(SnapshotId, SnapshotId)> {
    async fn resolve(store: &str, raw: &str) -> anyhow::Result<SnapshotId> {
        sekai_app::resolve_snapshot_ref(store, raw)
            .await
            .with_context(|| format!("snapshot {raw:?} failed to resolve"))
    }
    match (old_snapshot, new_snapshot) {
        (Some(old), Some(new)) => Ok((resolve(store, &old).await?, resolve(store, &new).await?)),
        (Some(old), None) => {
            let latest = sekai_app::latest_snapshot_id(store).await?;
            Ok((resolve(store, &old).await?, latest))
        }
        (None, new) => {
            let snapshots = sekai_app::list_snapshots(store).await?;
            if snapshots.len() < 2 {
                anyhow::bail!("at least 2 snapshots are required when snapshot IDs are omitted");
            }
            let old = snapshots[snapshots.len() - 2].id;
            let new_id = if let Some(raw) = new {
                resolve(store, &raw).await?
            } else {
                snapshots[snapshots.len() - 1].id
            };
            Ok((old, new_id))
        }
    }
}

fn scoped_coords(mut coords: Vec<ChunkCoord>, scope: &Scope) -> Vec<ChunkCoord> {
    coords.retain(|coord| scope.contains(*coord));
    coords.sort();
    coords.dedup();
    coords
}
