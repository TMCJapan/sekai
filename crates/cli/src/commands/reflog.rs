//! Reflog subcommand execution, DTOs, and output rendering
//! (ADR-0036 *reflog*).

use sekai_app::{Location, ReflogEntry};
use serde::Serialize;

use crate::cli::ReflogCommand;
use crate::envelope::Printer;

pub async fn run(
    location: &Location,
    command: &ReflogCommand,
    printer: &Printer,
) -> anyhow::Result<()> {
    let instance = sekai_app::SekaiInstance::open_location(location).await?;
    let log = instance.resolve_reflog(&command.log).await?;
    let entries = instance.read_reflog(&log, usize::MAX).await?;
    let selector = |n: usize| log.selector(n);
    if command.json {
        let listed: Vec<_> = entries
            .iter()
            .enumerate()
            .map(|(n, entry)| EntryJson::new(selector(n), entry))
            .collect();
        printer.print_ok("reflog", &listed)?;
        return Ok(());
    }
    for (n, entry) in entries.iter().enumerate() {
        println!("{} {}: {}", entry.new, selector(n), entry.message);
    }
    Ok(())
}

/// One reflog entry, newest first.
#[derive(Serialize)]
struct EntryJson {
    selector: String,
    old_commit: Option<String>,
    new_commit: String,
    message: String,
    at_ms: u64,
}

impl EntryJson {
    fn new(selector: String, entry: &ReflogEntry) -> Self {
        Self {
            selector,
            old_commit: entry.old.as_ref().map(ToString::to_string),
            new_commit: entry.new.to_string(),
            message: entry.message.clone(),
            at_ms: entry.at_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sekai_app::CommitId;

    #[test]
    fn renders_reflog_json() {
        let new = CommitId([5; 32]);
        let entry = ReflogEntry {
            old: None,
            new,
            message: "commit (initial): ".to_owned(),
            at_ms: 7,
        };
        assert_eq!(
            serde_json::to_string(&EntryJson::new("HEAD@{0}".to_owned(), &entry)).unwrap(),
            format!(
                r#"{{"selector":"HEAD@{{0}}","old_commit":null,"new_commit":"{new}","message":"commit (initial): ","at_ms":7}}"#
            )
        );
    }
}
