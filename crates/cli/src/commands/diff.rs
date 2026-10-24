//! Diff subcommand execution, DTOs, and coordination.

use anyhow::Context as _;
use sekai_app::{AppError, ChunkCoord, ChunkDiff, CommitId, HostWorldTree, Scope, SekaiStore};

use super::diff_render::{
    TimedDiffs, diff_entry_payload, diff_group_payload, print_diff_timing_table,
    render_diff_grouped, render_diff_human,
};
use super::progress::{finish_progress, progress_bar, report_progress};
use crate::cli::{CommitRef, DiffArgs, DiffTarget};
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
        DiffTarget::World { world, commit } => {
            let world = HostWorldTree::new(world)
                .with_context(|| format!("failed to inspect world {}", world.display()))?;
            if broad {
                coords.append(&mut sekai_app::world_chunk_coords(&world)?);
            }
            let coords = scoped_coords(coords, &scope);
            let commit_id = match commit {
                Some(raw) => Some(
                    instance
                        .resolve_commit_ref(raw.as_str())
                        .await
                        .with_context(|| format!("commit {raw} failed to resolve"))?,
                ),
                None => None,
            };
            instance
                .world(&world)
                .diff_world_chunks(commit_id, &coords, None, |update| {
                    report_progress(
                        bar.as_ref(),
                        update.chunks_done,
                        update.chunks_total,
                        "chunks",
                    );
                })
                .await
                .with_context(|| "failed to compute chunk diffs between world state and commit")?
        }
        DiffTarget::Commits { old, new } => {
            let (old_id, new_id) = resolve_commit_pair(&instance, old, new).await?;
            if broad {
                let mut union = instance.commit_chunk_coords(old_id).await?;
                union.extend(instance.commit_chunk_coords(new_id).await?);
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
                        "failed to compute chunk diffs between commit {} and {}",
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

async fn resolve_commit_pair(
    instance: &sekai_app::SekaiInstance<impl SekaiStore>,
    old_commit: Option<&CommitRef>,
    new_commit: Option<&CommitRef>,
) -> anyhow::Result<(CommitId, CommitId)> {
    async fn resolve(
        instance: &sekai_app::SekaiInstance<impl SekaiStore>,
        raw: &CommitRef,
    ) -> anyhow::Result<CommitId> {
        instance
            .resolve_commit_ref(raw.as_str())
            .await
            .with_context(|| format!("commit {raw} failed to resolve"))
    }
    async fn active_commit(
        instance: &sekai_app::SekaiInstance<impl SekaiStore>,
    ) -> anyhow::Result<CommitId> {
        match instance.active_ref().await? {
            None => Err(AppError::NoCommits.into()),
            Some(active) => match active.commit {
                Some(id) => Ok(id),
                None => anyhow::bail!(
                    "active ref {} is unborn (no commit yet); activate a born ref first",
                    active.name.as_str()
                ),
            },
        }
    }
    /// Commit parents are stored sorted by ID in both backends, so this
    /// returns the lowest-ID parent. For the single-parent commits that
    /// `commit` creates this is the continued commit; prune may produce
    /// merges whose parents are reported in ID order.
    async fn lowest_id_parent(
        instance: &sekai_app::SekaiInstance<impl SekaiStore>,
        commit: CommitId,
    ) -> anyhow::Result<CommitId> {
        instance
            .commit_parents(commit)
            .await?
            .into_iter()
            .next()
            .with_context(|| format!("commit {} has no parent to diff against", commit.raw()))
    }
    match (old_commit, new_commit) {
        (Some(old), Some(new)) => {
            Ok((resolve(instance, old).await?, resolve(instance, new).await?))
        }
        (Some(old), None) => {
            let new_id = active_commit(instance).await?;
            Ok((resolve(instance, old).await?, new_id))
        }
        (None, Some(new)) => {
            let new_id = resolve(instance, new).await?;
            Ok((lowest_id_parent(instance, new_id).await?, new_id))
        }
        (None, None) => {
            let new_id = active_commit(instance).await?;
            Ok((lowest_id_parent(instance, new_id).await?, new_id))
        }
    }
}

fn scoped_coords(mut coords: Vec<ChunkCoord>, scope: &Scope) -> Vec<ChunkCoord> {
    coords.retain(|coord| scope.contains(*coord));
    coords.sort();
    coords.dedup();
    coords
}
