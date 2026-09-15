//! Debug subcommands (`scan`) execution, DTOs, and output rendering.

use anyhow::Context as _;
use sekai_app::{RegionKind, ScanTimings};
use serde::Serialize;
use std::path::Path;

use crate::cli::TimingArgs;
use crate::envelope::envelope_ok;
use crate::style::Styler;

pub fn run_scan(world: &Path, output: TimingArgs, style: Styler) -> anyhow::Result<()> {
    let (entries, timings) =
        sekai_app::scan(world).with_context(|| format!("scan of {} failed", world.display()))?;
    if output.json {
        if output.timing {
            println!(
                "{}",
                envelope_ok(
                    "scan",
                    &TimedEntries {
                        entries: scan_payload(&entries),
                        timing: Some((&timings).into()),
                    }
                )?
            );
        } else {
            println!("{}", envelope_ok("scan", &scan_payload(&entries))?);
        }
        return Ok(());
    }
    for entry in &entries {
        let short_hash = entry
            .header_hash
            .char_indices()
            .nth(12)
            .map_or(entry.header_hash.as_str(), |(idx, _)| {
                &entry.header_hash[..idx]
            });

        println!(
            "{} dim={} kind={} region=r.{}.{} size={} mtime={} chunks={} header={}",
            entry.path.display(),
            entry.dim.raw(),
            kind_name(entry.kind),
            entry.region_x,
            entry.region_z,
            entry.file_bytes,
            entry
                .mtime_ms
                .map_or_else(|| "n/a".to_owned(), |ms| ms.to_string()),
            entry.chunks,
            short_hash,
        );
    }
    let total_bytes: u64 = entries.iter().map(|e| e.file_bytes).sum();
    let total_chunks: usize = entries.iter().map(|e| e.chunks).sum();
    println!(
        "{} files, {} bytes, {} chunks",
        entries.len(),
        total_bytes,
        total_chunks
    );
    if output.timing {
        print_scan_timing_table(&timings, entries.len(), style);
    }
    Ok(())
}

#[derive(Serialize)]
struct ScanEntryJson {
    path: String,
    dim: i32,
    kind: i32,
    region_x: i32,
    region_z: i32,
    size: u64,
    mtime_ms: Option<u64>,
    chunks: usize,
    header_hash: String,
}

#[derive(Serialize)]
struct ScanTimingJson {
    total_ms: u128,
    phases: ScanPhasesJson,
}

#[allow(clippy::struct_field_names)]
#[derive(Serialize)]
struct ScanPhasesJson {
    discover_ms: u128,
    read_ms: u128,
    parse_ms: u128,
}

#[derive(Serialize)]
struct TimedEntries {
    entries: Vec<ScanEntryJson>,
    #[serde(flatten)]
    timing: Option<ScanTimingJson>,
}

impl From<&ScanTimings> for ScanTimingJson {
    fn from(timings: &ScanTimings) -> Self {
        Self {
            total_ms: timings.total.as_millis(),
            phases: ScanPhasesJson {
                discover_ms: timings.discover.as_millis(),
                read_ms: timings.read.as_millis(),
                parse_ms: timings.parse.as_millis(),
            },
        }
    }
}

fn scan_payload(entries: &[sekai_app::RegionScanEntry]) -> Vec<ScanEntryJson> {
    entries
        .iter()
        .map(|entry| ScanEntryJson {
            path: entry.path.to_string_lossy().into_owned(),
            dim: entry.dim.raw(),
            kind: entry.kind.raw(),
            region_x: entry.region_x,
            region_z: entry.region_z,
            size: entry.file_bytes,
            mtime_ms: entry.mtime_ms,
            chunks: entry.chunks,
            header_hash: entry.header_hash.clone(),
        })
        .collect()
}

fn kind_name(kind: RegionKind) -> &'static str {
    if kind == RegionKind::REGION {
        "region"
    } else if kind == RegionKind::ENTITIES {
        "entities"
    } else if kind == RegionKind::POI {
        "poi"
    } else {
        "unknown"
    }
}

fn print_scan_timing_table(timings: &ScanTimings, files: usize, style: Styler) {
    println!(
        "{}",
        style.dim(&format!(
            "timing total={}ms discover={}ms read={}ms parse={}ms files={}",
            timings.total.as_millis(),
            timings.discover.as_millis(),
            timings.read.as_millis(),
            timings.parse.as_millis(),
            files,
        ))
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::time::Duration;

    #[test]
    fn renders_scan_timed_json() {
        let timings = ScanTimings {
            total: Duration::from_millis(30),
            discover: Duration::from_millis(1),
            read: Duration::from_millis(2),
            parse: Duration::from_millis(3),
        };
        let empty: Vec<ScanEntryJson> = Vec::new();
        assert_eq!(
            serde_json::to_string(&TimedEntries {
                entries: empty,
                timing: Some((&timings).into()),
            })
            .unwrap(),
            r#"{"entries":[],"total_ms":30,"phases":{"discover_ms":1,"read_ms":2,"parse_ms":3}}"#
        );
    }
}
