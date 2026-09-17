//! List subcommand execution, DTOs, and output rendering.

use crate::envelope::envelope_ok;
use crate::style::Styler;
use serde::Serialize;
use std::fmt::Write as _;

pub async fn run(store: &str, json: bool, stat: bool, style: Styler) -> anyhow::Result<()> {
    let snapshots = sekai_app::list_snapshots(store).await?;
    let tags = sekai_app::list_tags(store).await?;
    let mut stats = Vec::new();
    if stat {
        for snapshot in &snapshots {
            stats.push(sekai_app::snapshot_stats(store, snapshot.id).await?);
        }
    }
    if json {
        println!(
            "{}",
            envelope_ok(
                "list",
                &list_payload(&snapshots, &tags, stat.then_some(stats.as_slice()))
            )?
        );
        return Ok(());
    }
    for (index, snapshot) in snapshots.iter().enumerate() {
        let mut line = format!(
            "{}\t{}",
            style.bold(&snapshot.id.raw().to_string()),
            format_time(snapshot.created_at_ms)
        );
        let names: Vec<&str> = tags
            .iter()
            .filter(|tag| tag.snapshot == snapshot.id)
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
struct SnapshotJson {
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

fn list_payload(
    snapshots: &[sekai_app::Snapshot],
    tags: &[sekai_app::SnapshotTag],
    stats: Option<&[sekai_app::SnapshotStats]>,
) -> Vec<SnapshotJson> {
    snapshots
        .iter()
        .enumerate()
        .map(|(index, snapshot)| SnapshotJson {
            id: snapshot.id.raw(),
            created_at_ms: snapshot.created_at_ms,
            tags: tags
                .iter()
                .filter(|tag| tag.snapshot == snapshot.id)
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

fn format_time(created_at_ms: u64) -> String {
    let millis = i64::try_from(created_at_ms).unwrap_or(i64::MAX);
    chrono::DateTime::from_timestamp_millis(millis)
        .map_or_else(|| format!("{created_at_ms}ms"), |time| time.to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_times() {
        assert_eq!(format_time(0), "1970-01-01T00:00:00+00:00");
        assert_eq!(format_time(u64::MAX), format!("{}ms", u64::MAX));
    }

    #[test]
    fn renders_list_json() {
        let empty: Vec<sekai_app::Snapshot> = Vec::new();
        let no_tags: Vec<sekai_app::SnapshotTag> = Vec::new();
        let no_stats: Vec<sekai_app::SnapshotStats> = Vec::new();
        assert_eq!(
            serde_json::to_string(&list_payload(&empty, &no_tags, None)).unwrap(),
            "[]"
        );
        let snapshots = vec![
            sekai_app::Snapshot {
                id: sekai_app::SnapshotId(1),
                created_at_ms: 1_700_000_000_000,
            },
            sekai_app::Snapshot {
                id: sekai_app::SnapshotId(2),
                created_at_ms: 1_700_000_001_000,
            },
        ];
        assert_eq!(
            serde_json::to_string(&list_payload(&snapshots, &no_tags, None)).unwrap(),
            r#"[{"id":1,"created_at_ms":1700000000000,"tags":[]},{"id":2,"created_at_ms":1700000001000,"tags":[]}]"#
        );
        let tags = vec![sekai_app::SnapshotTag::new(
            sekai_app::TagName::parse("stable").unwrap(),
            sekai_app::SnapshotId(2),
            1_700_000_002_000,
        )];
        assert_eq!(
            serde_json::to_string(&list_payload(&snapshots, &tags, None)).unwrap(),
            r#"[{"id":1,"created_at_ms":1700000000000,"tags":[]},{"id":2,"created_at_ms":1700000001000,"tags":["stable"]}]"#
        );
        let stats = vec![
            sekai_app::SnapshotStats {
                fresh_chunks: 2,
                fresh_tombstones: 0,
                new_blobs: 1,
                effective_chunks: 2,
            },
            sekai_app::SnapshotStats {
                fresh_chunks: 0,
                fresh_tombstones: 1,
                new_blobs: 0,
                effective_chunks: 1,
            },
        ];
        assert_eq!(
            serde_json::to_string(&list_payload(&snapshots, &tags, Some(&stats))).unwrap(),
            r#"[{"id":1,"created_at_ms":1700000000000,"tags":[],"fresh_chunks":2,"fresh_tombstones":0,"new_blobs":1,"effective_chunks":2},{"id":2,"created_at_ms":1700000001000,"tags":["stable"],"fresh_chunks":0,"fresh_tombstones":1,"new_blobs":0,"effective_chunks":1}]"#
        );
        assert_eq!(
            serde_json::to_string(&list_payload(&snapshots, &tags, Some(&no_stats))).unwrap(),
            r#"[{"id":1,"created_at_ms":1700000000000,"tags":[]},{"id":2,"created_at_ms":1700000001000,"tags":["stable"]}]"#
        );
    }
}
