//! Log subcommand execution, DTOs, and output rendering.

use crate::envelope::envelope_ok;
use crate::style::Styler;
use serde::Serialize;
use std::fmt::Write as _;

pub async fn run(store: &str, json: bool, stat: bool, style: Styler) -> anyhow::Result<()> {
    let instance = sekai_app::SekaiInstance::open(store).await?;
    let commits = instance.list_commits().await?;
    let tags = instance.list_tags().await?;
    let mut stats = Vec::new();
    if stat {
        for commit in &commits {
            stats.push(instance.commit_stats(commit.id).await?);
        }
    }
    if json {
        println!(
            "{}",
            envelope_ok(
                "log",
                &log_payload(&commits, &tags, stat.then_some(stats.as_slice()))
            )?
        );
        return Ok(());
    }
    if commits.is_empty() {
        eprintln!("no commits found in store");
        return Ok(());
    }
    for (index, commit) in commits.iter().enumerate() {
        let mut line = format!(
            "{}\t{}",
            style.bold(&commit.id.raw().to_string()),
            super::format_time(commit.created_at_ms)
        );
        let names: Vec<&str> = tags
            .iter()
            .filter(|tag| tag.commit == commit.id)
            .map(|tag| tag.name.as_str())
            .collect();
        if !names.is_empty() {
            let _ = write!(line, " @{}", names.join(" @"));
        }
        if let Some(entry) = stats.get(index) {
            let _ = write!(
                line,
                "\tfresh={} tombstones={} new_blobs={} effective={}",
                entry.fresh_chunks, entry.fresh_tombstones, entry.new_blobs, entry.effective_chunks
            );
        }
        println!("{line}");
    }
    Ok(())
}

#[derive(Serialize)]
struct CommitJson {
    id: u64,
    created_at_ms: u64,
    tags: Vec<String>,
    #[serde(flatten)]
    stats: Option<StatsJson>,
}

#[derive(Serialize)]
struct StatsJson {
    fresh_chunks: usize,
    fresh_tombstones: usize,
    new_blobs: usize,
    effective_chunks: usize,
}

fn log_payload(
    commits: &[sekai_app::Commit],
    tags: &[sekai_app::CommitTag],
    stats: Option<&[sekai_app::CommitStats]>,
) -> Vec<CommitJson> {
    commits
        .iter()
        .enumerate()
        .map(|(index, commit)| CommitJson {
            id: commit.id.raw(),
            created_at_ms: commit.created_at_ms,
            tags: tags
                .iter()
                .filter(|tag| tag.commit == commit.id)
                .map(|tag| tag.name.as_str().to_owned())
                .collect(),
            stats: stats
                .and_then(|rows| rows.get(index))
                .map(|entry| StatsJson {
                    fresh_chunks: entry.fresh_chunks,
                    fresh_tombstones: entry.fresh_tombstones,
                    new_blobs: entry.new_blobs,
                    effective_chunks: entry.effective_chunks,
                }),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::format_time;

    #[test]
    fn formats_times() {
        assert_eq!(format_time(0), "1970-01-01T00:00:00+00:00");
        assert_eq!(
            format_time(u64::MAX),
            format!("{}ms", u64::MAX)
        );
    }

    #[test]
    fn renders_log_json() {
        let empty: Vec<sekai_app::Commit> = Vec::new();
        let no_tags: Vec<sekai_app::CommitTag> = Vec::new();
        let no_stats: Vec<sekai_app::CommitStats> = Vec::new();
        assert_eq!(
            serde_json::to_string(&log_payload(&empty, &no_tags, None)).unwrap(),
            "[]"
        );
        let commits = vec![
            sekai_app::Commit {
                id: sekai_app::CommitId(1),
                created_at_ms: 1_700_000_000_000,
                parents: vec![],
            },
            sekai_app::Commit {
                id: sekai_app::CommitId(2),
                created_at_ms: 1_700_000_001_000,
                parents: vec![sekai_app::CommitId(1)],
            },
        ];
        assert_eq!(
            serde_json::to_string(&log_payload(&commits, &no_tags, None)).unwrap(),
            r#"[{"id":1,"created_at_ms":1700000000000,"tags":[]},{"id":2,"created_at_ms":1700000001000,"tags":[]}]"#
        );
        let tags = vec![sekai_app::CommitTag::new(
            sekai_app::TagName::parse("stable").unwrap(),
            sekai_app::CommitId(2),
            1_700_000_002_000,
        )];
        assert_eq!(
            serde_json::to_string(&log_payload(&commits, &tags, None)).unwrap(),
            r#"[{"id":1,"created_at_ms":1700000000000,"tags":[]},{"id":2,"created_at_ms":1700000001000,"tags":["stable"]}]"#
        );
        let stats = vec![
            sekai_app::CommitStats {
                fresh_chunks: 2,
                fresh_tombstones: 0,
                new_blobs: 1,
                effective_chunks: 2,
            },
            sekai_app::CommitStats {
                fresh_chunks: 0,
                fresh_tombstones: 1,
                new_blobs: 0,
                effective_chunks: 1,
            },
        ];
        assert_eq!(
            serde_json::to_string(&log_payload(&commits, &tags, Some(&stats))).unwrap(),
            r#"[{"id":1,"created_at_ms":1700000000000,"tags":[],"fresh_chunks":2,"fresh_tombstones":0,"new_blobs":1,"effective_chunks":2},{"id":2,"created_at_ms":1700000001000,"tags":["stable"],"fresh_chunks":0,"fresh_tombstones":1,"new_blobs":0,"effective_chunks":1}]"#
        );
        assert_eq!(
            serde_json::to_string(&log_payload(&commits, &tags, Some(&no_stats))).unwrap(),
            r#"[{"id":1,"created_at_ms":1700000000000,"tags":[]},{"id":2,"created_at_ms":1700000001000,"tags":["stable"]}]"#
        );
    }
}
