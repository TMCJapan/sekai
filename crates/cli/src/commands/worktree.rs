//! Worktree subcommand execution, DTOs, and output rendering (ADR-0035).

use sekai_app::{AddedWorktree, Head, HeadState, Location, RefKind, WorktreeEntry};
use serde::Serialize;

use super::progress::Bars;
use super::{HeadJson, head_now_at};
use crate::cli::WorktreeCommand;
use crate::envelope::Printer;
use crate::style::Styler;

pub async fn run(
    location: &Location,
    action: &WorktreeCommand,
    printer: &Printer,
) -> anyhow::Result<()> {
    let style = printer.style;
    let mut instance = sekai_app::SekaiInstance::open_location(location).await?;
    match action {
        WorktreeCommand::Add {
            path,
            commit_ish,
            progress,
            json,
        } => {
            let bars = Bars::new(progress.show(*json));
            let added = instance
                .add_worktree(location, path, commit_ish.as_ref(), &bars)
                .await?;
            if *json {
                printer.print_ok("worktree", &WorktreeJson::added(&added))?;
                return Ok(());
            }
            // Git prints these on stderr.
            eprintln!("Preparing worktree ({})", preparing(&added));
            eprintln!("{}", head_now_at(&added.commit));
        }
        WorktreeCommand::List { json } => {
            let worktrees = instance.list_worktrees(&location.store).await?;
            if *json {
                let listed: Vec<_> = worktrees.iter().map(WorktreeJson::from).collect();
                printer.print_ok("worktree", &listed)?;
                return Ok(());
            }
            for line in list_lines(&worktrees, style) {
                println!("{line}");
            }
        }
        WorktreeCommand::Remove { worktree, json } => {
            let removed = instance.remove_worktree(&location.store, worktree).await?;
            if *json {
                printer.print_ok("worktree", &WorktreeJson::from(&removed))?;
            }
        }
    }
    Ok(())
}

/// What Git's `Preparing worktree (...)` says the new worktree checks
/// out.
fn preparing(added: &AddedWorktree) -> String {
    match &added.head {
        Head::Branch(branch) if added.created_branch => format!("new branch '{branch}'"),
        Head::Branch(branch) => format!("checking out '{branch}'"),
        Head::Detached(commit) => format!("detached HEAD {commit}"),
    }
}

/// `worktree list` as Git prints it: the path padded to the longest, the
/// commit, and the branch in brackets or `(detached HEAD)`. An unborn
/// branch shows the null ID, as Git does.
fn list_lines(worktrees: &[WorktreeEntry], style: Styler) -> Vec<String> {
    let paths: Vec<String> = worktrees
        .iter()
        .map(|worktree| worktree.path.display().to_string())
        .collect();
    let width = paths
        .iter()
        .map(|path| path.chars().count())
        .max()
        .unwrap_or(0);
    worktrees
        .iter()
        .zip(&paths)
        .map(|(worktree, path)| {
            let commit = worktree
                .head
                .commit()
                .map_or_else(|| "0".repeat(64), |commit| commit.to_string());
            let head = match &worktree.head {
                HeadState::Attached { branch, .. } | HeadState::Unborn { branch } => {
                    format!("[{}]", style.green(branch.as_str()))
                }
                HeadState::Detached { .. } => "(detached HEAD)".to_owned(),
            };
            format!("{path:<width$} {commit} {head}")
        })
        .collect()
}

/// A worktree record (ADR-0036 *Output*).
#[derive(Serialize)]
struct WorktreeJson {
    name: String,
    path: String,
    main: bool,
    head: HeadJson,
}

impl WorktreeJson {
    fn added(added: &AddedWorktree) -> Self {
        let head = match &added.head {
            Head::Branch(branch) => HeadJson {
                refname: Some(RefKind::Branch.full_name(branch)),
                commit: Some(added.commit.to_string()),
            },
            Head::Detached(commit) => HeadJson {
                refname: None,
                commit: Some(commit.to_string()),
            },
        };
        Self {
            name: added.name.to_string(),
            path: added.root.display().to_string(),
            main: false,
            head,
        }
    }
}

impl From<&WorktreeEntry> for WorktreeJson {
    fn from(worktree: &WorktreeEntry) -> Self {
        Self {
            name: worktree.name.to_string(),
            path: worktree.path.display().to_string(),
            main: worktree.main,
            head: HeadJson::from(&worktree.head),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sekai_app::{CommitId, RefName, WorktreeName};

    fn entry(path: &str, head: HeadState) -> WorktreeEntry {
        WorktreeEntry {
            name: WorktreeName::parse("w").unwrap(),
            path: path.into(),
            main: false,
            head,
        }
    }

    #[test]
    fn lists_as_git_does() {
        let commit = CommitId([6; 32]);
        let main = RefName::parse("main").unwrap();
        let lines = list_lines(
            &[
                entry(
                    "/srv/world",
                    HeadState::Attached {
                        branch: main.clone(),
                        commit,
                    },
                ),
                entry("/srv/a", HeadState::Detached { commit }),
                entry("/srv/unborn", HeadState::Unborn { branch: main }),
            ],
            Styler::disabled(),
        );
        assert_eq!(
            lines,
            [
                format!("/srv/world  {commit} [main]"),
                format!("/srv/a      {commit} (detached HEAD)"),
                format!("/srv/unborn {} [main]", "0".repeat(64)),
            ]
        );
    }

    #[test]
    fn renders_worktree_json() {
        let commit = CommitId([6; 32]);
        let json = WorktreeJson::from(&entry("/srv/a", HeadState::Detached { commit }));
        assert_eq!(
            serde_json::to_string(&json).unwrap(),
            format!(
                r#"{{"name":"w","path":"/srv/a","main":false,"head":{{"ref":null,"commit":"{commit}"}}}}"#
            )
        );
    }
}
