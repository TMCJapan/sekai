//! Diff subcommand execution, DTOs, and coordination.

use anyhow::Context as _;
use sekai_app::{ChunkCoord, ChunkDiff, Scope, SnapshotId};

use super::diff_render::{
    TimedDiffs, diff_entry_payload, diff_group_payload, print_diff_timing_table,
    render_diff_grouped, render_diff_human,
};
use super::progress::{finish_progress, progress_bar, report_progress};
use crate::cli::{DiffArgs, DiffTarget, SnapshotRef};
use crate::envelope::envelope_ok;
use crate::style::Styler;

pub async fn run(store: &str, args: &DiffArgs, style: Styler) -> anyhow::Result<()> {
    let selection = &args.selection;
    let scope = selection.owned_scope();
    let mut coords = selection.explicit_chunks();
    let instance = sekai_app::SekaiInstance::open(store).await?;
    let bar = progress_bar(args.progress);

    // Broad areas (whole dimensions, rectangles) and empty selections
    // resolve through enumeration; explicit chunks join the union.
    let broad = selection.has_broad_areas() || coords.is_empty();
    let (diffs, timings) = match args.target() {
        DiffTarget::World { world, snapshot } => {
            if broad {
                coords.append(&mut sekai_app::world_chunk_coords(world)?);
            }
            let coords = scoped_coords(coords, &scope);
            let snapshot_id = match snapshot {
                Some(raw) => Some(
                    instance
                        .resolve_snapshot_ref(raw.as_str())
                        .await
                        .with_context(|| format!("snapshot {raw} failed to resolve"))?,
                ),
                None => None,
            };
            instance
                .world(world)
                .diff_world_chunks(snapshot_id, &coords, None, |update| {
                    report_progress(
                        bar.as_ref(),
                        update.chunks_done,
                        update.chunks_total,
                        "chunks",
                    );
                })
                .await
                .with_context(|| "failed to compute chunk diffs between world state and snapshot")?
        }
        DiffTarget::Snapshots { old, new } => {
            let (old_id, new_id) = resolve_snapshot_pair(&instance, old, new).await?;
            if broad {
                let mut union = instance.snapshot_chunk_coords(old_id).await?;
                union.extend(instance.snapshot_chunk_coords(new_id).await?);
                coords.append(&mut union);
            }
            let coords = scoped_coords(coords, &scope);
            instance
                .diff_chunks(old_id, new_id, &coords, None, |update| {
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
        }
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
    instance: &sekai_app::SekaiInstance,
    old_snapshot: Option<&SnapshotRef>,
    new_snapshot: Option<&SnapshotRef>,
) -> anyhow::Result<(SnapshotId, SnapshotId)> {
    async fn resolve(
        instance: &sekai_app::SekaiInstance,
        raw: &SnapshotRef,
    ) -> anyhow::Result<SnapshotId> {
        instance
            .resolve_snapshot_ref(raw.as_str())
            .await
            .with_context(|| format!("snapshot {raw} failed to resolve"))
    }
    match (old_snapshot, new_snapshot) {
        (Some(old), Some(new)) => {
            Ok((resolve(instance, old).await?, resolve(instance, new).await?))
        }
        (Some(old), None) => {
            let latest = instance.latest_snapshot_id().await?;
            Ok((resolve(instance, old).await?, latest))
        }
        (None, new) => {
            let snapshots = instance.list_snapshots().await?;
            if snapshots.len() < 2 {
                anyhow::bail!("at least 2 snapshots are required when snapshot IDs are omitted");
            }
            let old = snapshots[snapshots.len() - 2].id;
            let new_id = match new {
                Some(raw) => resolve(instance, raw).await?,
                None => snapshots[snapshots.len() - 1].id,
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
