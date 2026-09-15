//! Diff rendering helpers for human text and JSON DTOs.

use sekai_app::{ChunkDiff, DiffTimings, NbtChange};
use serde::Serialize;

use crate::style::Styler;

#[derive(Serialize)]
pub struct DiffEntryJson {
    pub path: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub val: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new: Option<String>,
}

#[derive(Serialize)]
pub struct CoordJson {
    pub dim: i32,
    pub kind: i32,
    pub x: i32,
    pub z: i32,
}

#[derive(Serialize)]
pub struct DiffGroupJson {
    pub coord: CoordJson,
    pub entries: Vec<DiffEntryJson>,
}

pub fn diff_group_payload(diffs: &[&ChunkDiff]) -> Vec<DiffGroupJson> {
    diffs
        .iter()
        .map(|diff| DiffGroupJson {
            coord: CoordJson {
                dim: diff.coord.dim.raw(),
                kind: diff.coord.kind.raw(),
                x: diff.coord.x,
                z: diff.coord.z,
            },
            entries: diff_entry_payload(&diff.entries),
        })
        .collect()
}

pub fn diff_entry_payload(diffs: &[sekai_app::NbtDiffEntry]) -> Vec<DiffEntryJson> {
    diffs.iter().map(diff_entry_json).collect()
}

pub fn diff_entry_json(entry: &sekai_app::NbtDiffEntry) -> DiffEntryJson {
    let (kind, val, old, new) = match &entry.change {
        NbtChange::Added(v) => ("added", Some(format!("{v}")), None, None),
        NbtChange::Removed(v) => ("removed", Some(format!("{v}")), None, None),
        NbtChange::Modified { old, new } => (
            "modified",
            None,
            Some(format!("{old}")),
            Some(format!("{new}")),
        ),
    };
    DiffEntryJson {
        path: entry.path.clone(),
        kind,
        val,
        old,
        new,
    }
}

pub fn render_diff_human(
    cx: i32,
    cz: i32,
    diffs: &[sekai_app::NbtDiffEntry],
    show_values: bool,
    style: Styler,
) -> String {
    if diffs.is_empty() {
        return format!("No differences found for chunk ({cx}, {cz}).");
    }
    render_diff_entries(diffs, show_values, style)
}

pub fn render_diff_grouped(diffs: &[&ChunkDiff], show_values: bool, style: Styler) -> String {
    diffs
        .iter()
        .map(|diff| {
            let coord = diff.coord;
            let header = format!(
                "chunk {}/{} ({}, {}):",
                coord.dim, coord.kind, coord.x, coord.z
            );
            format!(
                "{}\n{}",
                style.bold(&header),
                render_diff_entries(&diff.entries, show_values, style)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn render_diff_entries(
    diffs: &[sekai_app::NbtDiffEntry],
    show_values: bool,
    style: Styler,
) -> String {
    diffs
        .iter()
        .map(|entry| {
            let (op, detail) = match &entry.change {
                NbtChange::Added(v) => (style.green("+"), show_values.then(|| format!("{v}"))),
                NbtChange::Removed(v) => (style.red("-"), show_values.then(|| format!("{v}"))),
                NbtChange::Modified { old, new } => (
                    style.yellow("~"),
                    show_values.then(|| format!("{old} -> {new}")),
                ),
            };
            detail.map_or_else(
                || format!("{op} {}", entry.path),
                |detail| format!("{op} {}: {}", entry.path, truncate_value(&detail)),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn truncate_value(rendered: &str) -> String {
    const MAX_VALUE_CHARS: usize = 500;
    if rendered.chars().count() <= MAX_VALUE_CHARS {
        return rendered.to_owned();
    }
    let truncated: String = rendered.chars().take(MAX_VALUE_CHARS).collect();
    format!("{truncated}...")
}

pub fn print_diff_timing_table(timings: &DiffTimings, chunks: usize, style: Styler) {
    println!(
        "{}",
        style.dim(&format!(
            "timing total={}ms blob_fetch={}ms decompress={}ms diff_compute={}ms chunks={}",
            timings.total.as_millis(),
            timings.blob_fetch.as_millis(),
            timings.decompress.as_millis(),
            timings.diff_compute.as_millis(),
            chunks,
        ))
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_diffs() -> Vec<sekai_app::NbtDiffEntry> {
        vec![
            sekai_app::NbtDiffEntry {
                path: "Status".to_owned(),
                change: NbtChange::Modified {
                    old: sekai_app::NbtValue::String("minecraft:full".to_owned()),
                    new: sekai_app::NbtValue::String("minecraft:empty".to_owned()),
                },
            },
            sekai_app::NbtDiffEntry {
                path: "xPos".to_owned(),
                change: NbtChange::Added(sekai_app::NbtValue::Int(3)),
            },
            sekai_app::NbtDiffEntry {
                path: "old_tag".to_owned(),
                change: NbtChange::Removed(sekai_app::NbtValue::Byte(1)),
            },
        ]
    }

    #[test]
    fn renders_diff_human_without_and_with_values() {
        assert_eq!(
            render_diff_human(10, -5, &sample_diffs(), false, Styler::disabled()),
            "~ Status\n+ xPos\n- old_tag"
        );
        assert_eq!(
            render_diff_human(10, -5, &sample_diffs(), true, Styler::disabled()),
            "~ Status: \"minecraft:full\" -> \"minecraft:empty\"\n+ xPos: 3\n- old_tag: 1b"
        );
    }

    #[test]
    fn truncates_huge_values() {
        let big = "x".repeat(600);
        let diffs = vec![sekai_app::NbtDiffEntry {
            path: "blob".to_owned(),
            change: NbtChange::Added(sekai_app::NbtValue::String(big)),
        }];
        let rendered = render_diff_human(0, 0, &diffs, true, Styler::disabled());
        assert!(rendered.starts_with("+ blob: \"xxx"));
        assert!(rendered.ends_with("..."));
    }

    #[test]
    fn renders_diff_json() {
        assert_eq!(
            serde_json::to_string(&diff_entry_payload(&sample_diffs())).unwrap(),
            r#"[{"path":"Status","type":"modified","old":"\"minecraft:full\"","new":"\"minecraft:empty\""},{"path":"xPos","type":"added","val":"3"},{"path":"old_tag","type":"removed","val":"1b"}]"#
        );
    }
}
