//! Tag subcommand execution, DTOs, and output rendering (ADR-0036 *tag*).

use sekai_app::{Location, Revision};

use super::RefJson;
use crate::cli::{TagAction, TagCommand};
use crate::envelope::Printer;

pub async fn run(
    location: &Location,
    command: &TagCommand,
    printer: &Printer,
) -> anyhow::Result<()> {
    let mut instance = sekai_app::SekaiInstance::open_location(location).await?;
    let json = command.json;
    match &command.action {
        TagAction::List { patterns } => {
            let tags = instance.list_tags(patterns).await?;
            if json {
                let listed: Vec<_> = tags
                    .iter()
                    .map(|tag| RefJson::tag(&tag.name.name, Some(tag.commit)))
                    .collect();
                printer.print_ok("tag", &listed)?;
                return Ok(());
            }
            for tag in &tags {
                println!("{}", tag.name.name);
            }
        }
        TagAction::Create {
            name,
            target,
            force,
        } => {
            let target = target.clone().unwrap_or_else(Revision::head);
            let created = instance.create_tag(name, &target, *force).await?;
            if json {
                printer.print_ok("tag", &RefJson::tag(name, Some(created.commit)))?;
            } else if let Some(previous) = created.previous
                && previous != created.commit
            {
                // Git reports a replaced tag only when it moved.
                println!("Updated tag '{name}' (was {previous})");
            }
        }
        TagAction::Delete { names } => {
            let mut deleted = Vec::new();
            for name in names {
                let commit = instance.delete_tag(name).await?;
                if !json {
                    println!("Deleted tag '{name}' (was {commit})");
                }
                deleted.push(RefJson::tag(name, Some(commit)));
            }
            if json {
                printer.print_ok("tag", &deleted)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sekai_app::{CommitId, RefName};

    #[test]
    fn renders_tag_json() {
        let commit = CommitId([4; 32]);
        let tag = RefJson::tag(&RefName::parse("v1").unwrap(), Some(commit));
        assert_eq!(
            serde_json::to_string(&tag).unwrap(),
            format!(r#"{{"name":"refs/tags/v1","commit":"{commit}"}}"#)
        );
    }
}
