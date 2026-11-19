//! Branch subcommand execution, DTOs, and output rendering
//! (ADR-0036 *branch*).

use sekai_app::{BranchEntry, HeadState, Location, RefKind, Revision};
use serde::Serialize;

use super::RefJson;
use crate::cli::{BranchAction, BranchCommand};
use crate::envelope::Printer;
use crate::style::Styler;

pub async fn run(
    location: &Location,
    command: &BranchCommand,
    printer: &Printer,
) -> anyhow::Result<()> {
    let style = printer.style;
    let mut instance = sekai_app::SekaiInstance::open_location(location).await?;
    let json = command.json;
    match &command.action {
        BranchAction::List { patterns } => {
            let head = instance.head().await?;
            let branches = instance.list_branches(patterns).await?;
            if json {
                let listed: Vec<_> = branches.iter().map(ListedJson::from).collect();
                printer.print_ok("branch", &listed)?;
                return Ok(());
            }
            if let Some(line) = head.as_ref().and_then(detached_entry) {
                println!("* {}", style.green(&line));
            }
            for entry in &branches {
                println!("{}", list_line(entry, style));
            }
        }
        BranchAction::Create { name, start, force } => {
            let start = start.clone().unwrap_or_else(Revision::head);
            let created = instance.create_branch(name, &start, *force).await?;
            if json {
                let payload = RefJson::branch(name, Some(created.commit));
                printer.print_ok("branch", &payload)?;
            }
        }
        BranchAction::Delete { names, force } => {
            let mut deleted = Vec::new();
            for name in names {
                let commit = instance.delete_branch(name, *force).await?;
                if !json {
                    println!("Deleted branch {name} (was {commit}).");
                }
                deleted.push(RefJson::branch(name, Some(commit)));
            }
            if json {
                printer.print_ok("branch", &deleted)?;
            }
        }
        BranchAction::Rename { old, new, force } => {
            let renamed = instance.rename_branch(old.as_ref(), new, *force).await?;
            if json {
                let payload = RenamedJson {
                    branch: RefJson::branch(new, renamed.commit),
                    old_name: RefKind::Branch.full_name(&renamed.old),
                };
                printer.print_ok("branch", &payload)?;
            }
        }
        BranchAction::ShowCurrent => {
            let current = instance.head_branch().await?;
            if json {
                let payload = CurrentJson {
                    name: current.map(|branch| RefKind::Branch.full_name(&branch)),
                };
                printer.print_ok("branch", &payload)?;
            } else if let Some(current) = current {
                println!("{current}");
            }
        }
    }
    Ok(())
}

/// The entry Git lists first for a detached HEAD: `(HEAD detached at
/// <id>)`, where `<id>` is HEAD's commit (ADR-0036 *branch*).
fn detached_entry(head: &HeadState) -> Option<String> {
    match head {
        HeadState::Detached { commit } => Some(format!("(HEAD detached at {commit})")),
        HeadState::Attached { .. } | HeadState::Unborn { .. } => None,
    }
}

/// A listed branch as Git prints it: `*` for HEAD's branch, `+` for one
/// checked out in another worktree.
fn list_line(entry: &BranchEntry, style: Styler) -> String {
    let name = entry.branch.name.name.as_str();
    match (entry.head, &entry.worktree) {
        (true, _) => format!("* {}", style.green(name)),
        (false, Some(_)) => format!("+ {}", style.cyan(name)),
        (false, None) => format!("  {name}"),
    }
}

/// A branch a command created, moved, or deleted.
#[derive(Serialize)]
struct ListedJson {
    #[serde(flatten)]
    branch: RefJson,
    worktree: Option<String>,
}

impl From<&BranchEntry> for ListedJson {
    fn from(entry: &BranchEntry) -> Self {
        Self {
            branch: RefJson::branch(&entry.branch.name.name, Some(entry.branch.commit)),
            worktree: entry.worktree.as_ref().map(ToString::to_string),
        }
    }
}

#[derive(Serialize)]
struct RenamedJson {
    #[serde(flatten)]
    branch: RefJson,
    old_name: String,
}

#[derive(Serialize)]
struct CurrentJson {
    name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sekai_app::RefName;
    use sekai_app::{CommitId, CommitRef, Ref, WorktreeName};

    fn name(raw: &str) -> RefName {
        RefName::parse(raw).unwrap()
    }

    #[test]
    fn marks_branches_as_git_does() {
        let entry = |head, worktree: Option<&str>| BranchEntry {
            branch: CommitRef::new(Ref::branch(name("dev")), CommitId([1; 32])),
            worktree: worktree.map(|w| WorktreeName::parse(w).unwrap()),
            head,
        };
        let style = Styler::disabled();
        assert_eq!(list_line(&entry(true, Some("main")), style), "* dev");
        assert_eq!(list_line(&entry(false, Some("other")), style), "+ dev");
        assert_eq!(list_line(&entry(false, None), style), "  dev");
    }

    #[test]
    fn lists_a_detached_head_first() {
        let commit = CommitId([2; 32]);
        let id = commit.to_string();
        assert_eq!(
            detached_entry(&HeadState::Detached { commit }).as_deref(),
            Some(format!("(HEAD detached at {id})").as_str())
        );
        let attached = HeadState::Attached {
            branch: name("main"),
            commit,
        };
        assert_eq!(detached_entry(&attached), None);
    }

    #[test]
    fn renders_branch_json() {
        let commit = CommitId([3; 32]);
        let entry = BranchEntry {
            branch: CommitRef::new(Ref::branch(name("dev")), commit),
            worktree: Some(WorktreeName::parse("w").unwrap()),
            head: false,
        };
        assert_eq!(
            serde_json::to_string(&ListedJson::from(&entry)).unwrap(),
            format!(r#"{{"name":"refs/heads/dev","commit":"{commit}","worktree":"w"}}"#)
        );
        let renamed = RenamedJson {
            branch: RefJson::branch(&name("new"), None),
            old_name: Ref::branch(name("old")).full_name(),
        };
        assert_eq!(
            serde_json::to_string(&renamed).unwrap(),
            r#"{"name":"refs/heads/new","commit":null,"old_name":"refs/heads/old"}"#
        );
    }
}
