//! Branch subcommand execution, DTOs, and output rendering.

use anyhow::Context as _;
use sekai_app::{ListedBranch, Ref};
use sekai_storage_local::LocalStore;
use serde::Serialize;

use crate::cli::BranchArgs;
use crate::envelope::envelope_ok;
use crate::style::Styler;

pub async fn run(store: &str, args: &BranchArgs, style: Styler) -> anyhow::Result<()> {
    let instance = sekai_app::SekaiInstance::<LocalStore>::open(store).await?;
    let branches: Vec<ListedBranch> = instance
        .list_branches()
        .await
        .with_context(|| format!("branch list for {store} failed"))?;
    if args.json {
        let listed: Vec<BranchJson> = branches.iter().map(BranchJson::from).collect();
        println!("{}", envelope_ok("branch", &listed)?);
        return Ok(());
    }
    for branch in &branches {
        println!("{}", list_line(branch, style));
    }
    Ok(())
}

/// A listed branch as Git prints it: `* ` and green for HEAD's branch,
/// two spaces for the others. HEAD's branch is listed even while unborn
/// (deviation: Git has no ref to list then).
fn list_line(branch: &ListedBranch, style: Styler) -> String {
    let name = branch.name.as_str();
    if branch.head {
        format!("* {}", style.green(name))
    } else {
        format!("  {name}")
    }
}

/// A listed branch: its full refname, its commit (`null` while unborn),
/// and whether it is HEAD's branch.
#[derive(Serialize)]
struct BranchJson {
    name: String,
    commit: Option<String>,
    head: bool,
}

impl From<&ListedBranch> for BranchJson {
    fn from(branch: &ListedBranch) -> Self {
        Self {
            name: Ref::branch(branch.name.clone()).full_name(),
            commit: branch.commit.map(|commit| commit.to_string()),
            head: branch.head,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sekai_app::{CommitId, RefName};

    fn branch(name: &str, commit: Option<CommitId>, head: bool) -> ListedBranch {
        ListedBranch {
            name: RefName::parse(name).unwrap(),
            commit,
            head,
        }
    }

    #[test]
    fn marks_heads_branch_as_git_does() {
        let style = Styler::disabled();
        let commit = Some(CommitId([1; 32]));
        assert_eq!(list_line(&branch("main", commit, true), style), "* main");
        assert_eq!(list_line(&branch("main", None, true), style), "* main");
        assert_eq!(
            list_line(&branch("feature/x", commit, false), style),
            "  feature/x"
        );
    }

    #[test]
    fn renders_branch_json() {
        let commit = CommitId([3; 32]);
        assert_eq!(
            serde_json::to_string(&BranchJson::from(&branch("dev", Some(commit), false))).unwrap(),
            format!(r#"{{"name":"refs/heads/dev","commit":"{commit}","head":false}}"#)
        );
        assert_eq!(
            serde_json::to_string(&BranchJson::from(&branch("main", None, true))).unwrap(),
            r#"{"name":"refs/heads/main","commit":null,"head":true}"#
        );
    }
}
