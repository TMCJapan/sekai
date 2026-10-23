//! Tag subcommand execution, DTOs, and output rendering.

use anyhow::Context as _;
use sekai_app::CommitTag;
use serde::Serialize;

use crate::cli::TagCommand;
use crate::envelope::envelope_ok;
use crate::style::Styler;

pub async fn run(store: &str, action: &TagCommand, style: Styler) -> anyhow::Result<()> {
    let mut instance = sekai_app::SekaiInstance::open(store).await?;

    match action {
        TagCommand::Create {
            name,
            commit,
            force,
            json,
        } => {
            let id = instance
                .resolve_commit_ref(commit.as_str())
                .await
                .with_context(|| format!("tag target commit {commit} failed to resolve"))?;
            let record = instance
                .create_tag(name, id, *force)
                .await
                .with_context(|| format!("tag {name} failed"))?;
            if *json {
                println!("{}", envelope_ok("tag", &tag_payload(&record))?);
                return Ok(());
            }
            println!(
                "tagged {} as {}",
                style.bold(&record.commit.raw().to_string()),
                style.bold(name.as_str())
            );
        }
        TagCommand::Delete { name, json } => {
            let removed = instance
                .delete_tag(name)
                .await
                .with_context(|| format!("delete of tag {name} failed"))?;
            if !removed {
                anyhow::bail!("unknown tag: {name}");
            }
            if *json {
                println!(
                    "{}",
                    envelope_ok(
                        "tag",
                        &TagDelete {
                            name: name.as_str().to_owned(),
                        }
                    )?
                );
                return Ok(());
            }
            println!("deleted tag {}", style.bold(name.as_str()));
        }
        TagCommand::List { json } => {
            let tags = instance
                .list_tags()
                .await
                .with_context(|| format!("tag list for {store} failed"))?;
            if *json {
                println!(
                    "{}",
                    envelope_ok("tag", &tags.iter().map(tag_payload).collect::<Vec<_>>())?
                );
                return Ok(());
            }
            for tag in &tags {
                println!(
                    "{}\t{}\t{}",
                    style.bold(tag.name.as_str()),
                    tag.commit.raw(),
                    super::format_time(tag.created_at_ms)
                );
            }
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct TagJson {
    name: String,
    commit: u64,
    created_at_ms: u64,
}

#[derive(Serialize)]
struct TagDelete {
    name: String,
}

fn tag_payload(tag: &CommitTag) -> TagJson {
    TagJson {
        name: tag.name.as_str().to_owned(),
        commit: tag.commit.raw(),
        created_at_ms: tag.created_at_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sekai_app::TagName;

    #[test]
    fn renders_tag_payloads() {
        let tag = CommitTag::new(
            TagName::parse("stable").unwrap(),
            sekai_app::CommitId(3),
            1_700_000_000_000,
        );
        assert_eq!(
            serde_json::to_string(&tag_payload(&tag)).unwrap(),
            r#"{"name":"stable","commit":3,"created_at_ms":1700000000000}"#
        );
        let tags: Vec<TagJson> = Vec::new();
        assert_eq!(serde_json::to_string(&tags).unwrap(), "[]");
    }
}
