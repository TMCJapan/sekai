//! List subcommand execution, DTOs, and output rendering.

use crate::envelope::envelope_ok;
use crate::style::Styler;
use serde::Serialize;

pub async fn run(store: &str, json: bool, style: Styler) -> anyhow::Result<()> {
    let snapshots = sekai_app::list_snapshots(store).await?;
    if json {
        println!("{}", envelope_ok("list", &list_payload(&snapshots))?);
        return Ok(());
    }
    for snapshot in &snapshots {
        println!(
            "{}\t{}",
            style.bold(&snapshot.id.raw().to_string()),
            format_time(snapshot.created_at_ms)
        );
    }
    Ok(())
}

#[derive(Serialize)]
struct SnapshotJson {
    id: u64,
    created_at_ms: u64,
}

fn list_payload(snapshots: &[sekai_app::Snapshot]) -> Vec<SnapshotJson> {
    snapshots
        .iter()
        .map(|snapshot| SnapshotJson {
            id: snapshot.id.raw(),
            created_at_ms: snapshot.created_at_ms,
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
        assert_eq!(serde_json::to_string(&list_payload(&empty)).unwrap(), "[]");
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
            serde_json::to_string(&list_payload(&snapshots)).unwrap(),
            r#"[{"id":1,"created_at_ms":1700000000000},{"id":2,"created_at_ms":1700000001000}]"#
        );
    }
}
