//! Diff rendering helpers for human text and JSON DTOs.

// Compatibility aliases for renderer names used by callers/tests.

use sekai_app::{ChunkDiff, DiffTimings};
use serde::Serialize;

use crate::style::Styler;

#[derive(Serialize)]
pub struct CoordJson {
    pub dim: i32,
    pub kind: u8,
    pub x: i32,
    pub z: i32,
}

#[derive(Serialize)]
pub struct DiffGroupJson {
    pub coord: CoordJson,
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
        })
        .collect()
}

/// Render the coordinates of changed chunks, one per line.
pub fn render_diff_coords(diffs: &[&ChunkDiff], style: Styler) -> String {
    diffs
        .iter()
        .map(|diff| {
            style.bold(&format!(
                "{}/{} {},{}",
                diff.coord.dim, diff.coord.kind, diff.coord.x, diff.coord.z
            ))
        })
        .collect::<Vec<_>>()
        .join("\n")
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

#[derive(Serialize)]
pub struct DiffTimingJson {
    pub total_ms: u128,
    pub phases: DiffPhasesJson,
}

#[allow(clippy::struct_field_names)]
#[derive(Serialize)]
pub struct DiffPhasesJson {
    pub blob_fetch_ms: u128,
    pub decompress_ms: u128,
    pub diff_compute_ms: u128,
}

#[derive(Serialize)]
pub struct TimedDiffs<T> {
    pub diffs: T,
    #[serde(flatten)]
    pub timing: Option<DiffTimingJson>,
}

impl From<&DiffTimings> for DiffTimingJson {
    fn from(timings: &DiffTimings) -> Self {
        Self {
            total_ms: timings.total.as_millis(),
            phases: DiffPhasesJson {
                blob_fetch_ms: timings.blob_fetch.as_millis(),
                decompress_ms: timings.decompress.as_millis(),
                diff_compute_ms: timings.diff_compute.as_millis(),
            },
        }
    }
}


