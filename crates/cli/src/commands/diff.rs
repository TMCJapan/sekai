//! Diff subcommand execution, DTOs, and coordination.

use sekai_app::{ChunkDiff, Location, scoped_coords};

use super::diff_render::{
    TimedDiffs, diff_group_payload, print_diff_timing_table, render_diff_coords,
};
use super::progress::Bars;
use super::resolve_selection;
use crate::cli::{DiffArgs, DiffTarget, Selection};
use crate::envelope::Printer;

pub async fn run(location: &Location, args: &DiffArgs, printer: &Printer) -> anyhow::Result<()> {
    let style = printer.style;
    // Broad areas (whole dimensions, rectangles) and empty selections
    // resolve through enumeration; explicit chunks join the union.
    let (mut coords, broad) = match &args.selection {
        Selection::Inline(inline) => {
            let coords = inline.explicit_chunks();
            let broad = inline.has_broad_areas() || coords.is_empty();
            (coords, broad)
        }
        // A named scope spells no chunk inline, so it always enumerates and
        // filters.
        Selection::Scope(_) => (Vec::new(), true),
    };
    let instance = sekai_app::SekaiInstance::open_location(location).await?;
    let scope = resolve_selection(&instance, &args.selection).await?;
    let bars = Bars::new(args.output.progress.show(args.output.json));

    let (diffs, timings) = match &args.target {
        DiffTarget::World(commit) => {
            // Resolve the commit before scanning the world: a bad
            // reference must fail without waiting on a full world read.
            let world = location.world_tree()?;
            let commit_id = instance.resolve_diff_commit(commit.as_ref()).await?;
            if broad {
                coords.append(&mut sekai_app::world_chunk_coords(&world)?);
            }
            let coords = scoped_coords(coords, &scope);
            instance
                .world(&world)
                .diff_world_chunks(Some(commit_id), &coords, &bars)
                .await?
        }
        DiffTarget::Commits { old, new } => {
            let old_id = instance.resolve_diff_commit(Some(old)).await?;
            let new_id = instance.resolve_diff_commit(Some(new)).await?;
            if broad {
                let mut union = instance.commit_chunk_coords(old_id).await?;
                union.extend(instance.commit_chunk_coords(new_id).await?);
                coords.append(&mut union);
            }
            let coords = scoped_coords(coords, &scope);
            instance.diff_chunks(old_id, new_id, &coords, &bars).await?
        }
    };

    let nonempty: Vec<&ChunkDiff> = diffs
        .iter()
        .filter(|diff| !diff.entries.is_empty())
        .collect();
    if args.output.json {
        if args.output.timing {
            printer.print_ok(
                "diff",
                &TimedDiffs {
                    diffs: diff_group_payload(&nonempty),
                    timing: Some((&timings).into()),
                },
            )?;
        } else {
            printer.print_ok("diff", &diff_group_payload(&nonempty))?;
        }
        return Ok(());
    }
    if nonempty.is_empty() {
        println!("No differences found.");
        return Ok(());
    }
    println!("{}", render_diff_coords(&nonempty, style));
    if args.output.timing {
        print_diff_timing_table(&timings, diffs.len(), style);
    }
    Ok(())
}
