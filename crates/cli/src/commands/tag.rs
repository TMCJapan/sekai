//! Tag subcommand execution, DTOs, and output rendering.

use anyhow::Context as _;
use sekai_app::{SnapshotTag, TagName};
use serde::Serialize;

use crate::envelope::envelope_ok;
use crate::style::Styler;

pub async fn run(
    store: &str,
    name: Option<TagName>,
    snapshot: Option<String>,
    delete: bool,
    force: bool,
    json: bool,
    style: Styler,
) -> anyhow::Result<()> {
    if delete {
        let Some(name) = name else {
            anyhow::bail!("tag name is required to delete a tag");
        };
        let removed = sekai_app::delete_tag(store, &name)
            .await
            .with_context(|| format!("delete of tag {name} failed"))?;
        if !removed {
            anyhow::bail!("unknown tag: {name}");
        }
        if json {
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
        return Ok(());
    }
    if let (Some(name), Some(snapshot)) = (name.as_ref(), snapshot.as_ref()) {
        let id = sekai_app::resolve_snapshot_ref(store, snapshot)
            .await
            .with_context(|| format!("tag target {snapshot:?} failed to resolve"))?;
        let record = sekai_app::create_tag(store, name, id, force)
            .await
            .with_context(|| format!("tag {name} failed"))?;
        if json {
            println!("{}", envelope_ok("tag", &tag_payload(&record))?);
            return Ok(());
        }
        println!(
            "tagged {} as {}",
            style.bold(&record.snapshot.raw().to_string()),
            style.bold(name.as_str())
        );
        return Ok(());
    }
    if name.is_some() || snapshot.is_some() {
        anyhow::bail!("tag creation needs both <name> and <snapshot>");
    }
    let tags = sekai_app::list_tags(store)
        .await
        .with_context(|| format!("tag list for {store} failed"))?;
    if json {
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
            tag.snapshot.raw(),
            format_time(tag.created_at_ms)
        );
    }
    Ok(())
}

#[derive(Serialize)]
struct TagJson {
    name: String,
    snapshot: u64,
    created_at_ms: u64,
}

#[derive(Serialize)]
struct TagDelete {
    name: String,
}

fn tag_payload(tag: &SnapshotTag) -> TagJson {
    TagJson {
        name: tag.name.as_str().to_owned(),
        snapshot: tag.snapshot.raw(),
        created_at_ms: tag.created_at_ms,
    }
}

fn format_time(created_at_ms: u64) -> String {
    let millis = i64::try_from(created_at_ms).unwrap_or(i64::MAX);
    chrono::DateTime::from_timestamp_millis(millis)
        .map_or_else(|| format!("{created_at_ms}ms"), |time| time.to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_tag_payloads() {
        let tag = SnapshotTag::new(
            TagName::parse("stable").unwrap(),
            sekai_app::SnapshotId(3),
            1_700_000_000_000,
        );
        assert_eq!(
            serde_json::to_string(&tag_payload(&tag)).unwrap(),
            r#"{"name":"stable","snapshot":3,"created_at_ms":1700000000000}"#
        );
        let tags: Vec<TagJson> = Vec::new();
        assert_eq!(serde_json::to_string(&tags).unwrap(), "[]");
    }
}
