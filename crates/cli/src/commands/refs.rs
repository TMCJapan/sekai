//! Ref subcommand execution, DTOs, and output rendering.

use sekai_app::CommitRef;
use serde::Serialize;

use crate::cli::RefCommand;
use crate::envelope::envelope_ok;
use crate::style::Styler;
use anyhow::Context as _;

pub async fn run(store: &str, action: &RefCommand, style: Styler) -> anyhow::Result<()> {
    let mut instance = sekai_app::SekaiInstance::open(store).await?;

    match action {
        RefCommand::Create {
            name,
            commit,
            force,
            json,
        } => {
            // Without an explicit target the ref starts where the active
            // ref stands (possibly unborn, yielding an unborn ref); an
            // explicit target must already name a stored commit.
            let target = match commit {
                Some(reference) => Some(
                    instance
                        .resolve_commit_ref(reference.as_str())
                        .await
                        .with_context(|| {
                            format!("ref target commit {reference} failed to resolve")
                        })?,
                ),
                None => instance
                    .current_commit_id()
                    .await
                    .with_context(|| format!("ref target of {store} failed to resolve"))?,
            };
            let record = instance
                .create_ref(name, target, *force)
                .await
                .with_context(|| format!("ref {name} failed"))?;
            if *json {
                println!("{}", envelope_ok("ref", &ref_payload(&record))?);
                return Ok(());
            }
            match record.commit {
                Some(commit) => println!(
                    "ref {} points at {}",
                    style.bold(record.name.as_str()),
                    style.bold(&commit.raw().to_string())
                ),
                None => println!(
                    "ref {} is unborn (no commit yet)",
                    style.bold(record.name.as_str())
                ),
            }
        }
        RefCommand::Delete { name, json } => {
            let removed = instance
                .delete_ref(name)
                .await
                .with_context(|| format!("delete of ref {name} failed"))?;
            if !removed {
                anyhow::bail!("unknown ref: {name}");
            }
            if *json {
                println!(
                    "{}",
                    envelope_ok(
                        "ref",
                        &RefDelete {
                            name: name.as_str().to_owned(),
                        }
                    )?
                );
                return Ok(());
            }
            println!("deleted ref {}", style.bold(name.as_str()));
        }
        RefCommand::List { json } => {
            let refs = instance
                .list_refs()
                .await
                .with_context(|| format!("ref list for {store} failed"))?;
            if *json {
                println!(
                    "{}",
                    envelope_ok("ref", &refs.iter().map(ref_payload).collect::<Vec<_>>())?
                );
                return Ok(());
            }
            for record in &refs {
                // Refs come in name order; the active one carries the `*`
                // marker wherever it sorts.
                println!(
                    "{}{}\t{}\t{}",
                    if record.active { "* " } else { "  " },
                    style.bold(record.name.as_str()),
                    record
                        .commit
                        .map_or_else(|| "-".to_owned(), |commit| commit.raw().to_string()),
                    super::format_time(record.created_at_ms)
                );
            }
        }
        RefCommand::Activate { name, json } => {
            let record = instance
                .activate_ref(name)
                .await
                .with_context(|| format!("activation of ref {name} failed"))?;
            if *json {
                println!("{}", envelope_ok("ref", &ref_payload(&record))?);
                return Ok(());
            }
            println!(
                "activated ref {} (new commits hang from it)",
                style.bold(record.name.as_str())
            );
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct RefJson {
    name: String,
    commit: Option<u64>,
    active: bool,
    created_at_ms: u64,
}

#[derive(Serialize)]
struct RefDelete {
    name: String,
}

fn ref_payload(record: &CommitRef) -> RefJson {
    RefJson {
        name: record.name.as_str().to_owned(),
        commit: record.commit.map(sekai_app::CommitId::raw),
        active: record.active,
        created_at_ms: record.created_at_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sekai_app::{CommitId, RefName};

    #[test]
    fn renders_ref_payloads() {
        let record = CommitRef::new(
            RefName::parse("main").unwrap(),
            Some(CommitId(3)),
            true,
            1_700_000_000_000,
        );
        assert_eq!(
            serde_json::to_string(&ref_payload(&record)).unwrap(),
            r#"{"name":"main","commit":3,"active":true,"created_at_ms":1700000000000}"#
        );
        let unborn = CommitRef::new(RefName::parse("feature").unwrap(), None, false, 0);
        assert_eq!(
            serde_json::to_string(&ref_payload(&unborn)).unwrap(),
            r#"{"name":"feature","commit":null,"active":false,"created_at_ms":0}"#
        );
    }
}
