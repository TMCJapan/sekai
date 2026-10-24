//! Commit subcommand execution, DTOs, and output rendering.

use anyhow::Context as _;
use sekai_app::{CommitOptions, CommitReport, CommitTimings, HostWorldTree};
use serde::Serialize;
use std::path::Path;

use super::ReportOut;
use super::progress::{finish_progress, progress_bar, report_progress};
use crate::cli::Selection;
use crate::envelope::envelope_ok;
use crate::style::Styler;

const fn options(with_diff: bool, jobs: usize) -> CommitOptions {
    CommitOptions {
        concurrency: jobs,
        with_diff,
        ignore_tags: None,
    }
}

/// Whether the store directory already exists. Unknown URL schemes are
/// treated as pre-existing so no note is printed for backends this
/// helper cannot resolve to a path.
fn store_preexists(store: &str) -> bool {
    let path = match store.strip_prefix("sqlite://") {
        Some(rest) => rest,
        None if store.contains("://") => return true,
        None => store,
    };
    std::path::Path::new(path).exists()
}

pub async fn run(
    store: &str,
    world_path: impl AsRef<Path>,
    with_diff: bool,
    jobs: usize,
    progress: bool,
    selection: &Selection,
    out: ReportOut,
) -> anyhow::Result<()> {
    let scope = selection.owned_scope();
    let fresh_store = !store_preexists(store);
    let mut world = HostWorldTree::new(&world_path)
        .with_context(|| format!("failed to inspect world {}", world_path.as_ref().display()))?;
    let bar = progress_bar(progress);
    let mut instance = sekai_app::SekaiInstance::init(store)
        .await
        .with_context(|| format!("commit of {} failed", world_path.as_ref().display()))?;
    let (report, timings) = instance
        .world_mut(&mut world)
        .commit(options(with_diff, jobs), scope, |update| {
            report_progress(
                bar.as_ref(),
                update.files_done,
                update.files_total,
                format!("files {} chunks", update.chunks_done),
            );
        })
        .await
        .with_context(|| format!("commit of {} failed", world_path.as_ref().display()))?;
    finish_progress(bar.as_ref());
    if out.json {
        println!(
            "{}",
            envelope_ok("commit", &commit_payload(&report, &timings, out.timing))?
        );
        return Ok(());
    }
    if fresh_store {
        eprintln!("store created at {store}");
    }
    let style = out.style;
    println!(
        "commit {} recorded: {} chunks, {} new blobs, {} tombstones",
        style.bold(&report.commit.to_string()),
        report.chunks,
        report.new_blobs,
        report.tombstones
    );
    if out.timing {
        print_timing_table(&timings, style);
    }
    Ok(())
}

#[derive(Serialize)]
struct CommitPayload {
    commit: String,
    chunks: usize,
    new_blobs: usize,
    tombstones: usize,
    skipped_regions: usize,
    carried_chunks: usize,
    #[serde(flatten)]
    timing: Option<CommitTiming>,
}

#[derive(Serialize)]
struct CommitTiming {
    total_ms: u128,
    phases: CommitPhases,
    regions: Vec<RegionTimingJson>,
}

#[allow(clippy::struct_field_names)]
#[derive(Serialize)]
struct CommitPhases {
    discover_ms: u128,
    universe_load_ms: u128,
    fingerprint_ms: u128,
    region_open_ms: u128,
    ingest_ms: u128,
    hash_ms: u128,
    cas_put_ms: u128,
    db_apply_ms: u128,
}

#[derive(Serialize)]
struct RegionTimingJson {
    path: String,
    bytes: u64,
    chunks: usize,
    open_ms: u128,
    ingest_ms: u128,
    hash_ms: u128,
    cas_ms: u128,
}

fn commit_payload(report: &CommitReport, timings: &CommitTimings, timing: bool) -> CommitPayload {
    CommitPayload {
        commit: report.commit.to_string(),
        chunks: report.chunks,
        new_blobs: report.new_blobs,
        tombstones: report.tombstones,
        skipped_regions: report.skipped_regions,
        carried_chunks: report.carried_chunks,
        // `then`, not `then_some`: `then` evaluates its closure lazily,
        // while `then_some` eagerly builds the timing block (and a copy of
        // every region's location) even without `--timing`.
        timing: timing.then(|| CommitTiming {
            total_ms: timings.total.as_millis(),
            phases: CommitPhases {
                discover_ms: timings.discover.as_millis(),
                universe_load_ms: timings.universe_load.as_millis(),
                fingerprint_ms: timings.fingerprint.as_millis(),
                region_open_ms: timings.region_open.as_millis(),
                ingest_ms: timings.ingest.as_millis(),
                hash_ms: timings.hash.as_millis(),
                cas_put_ms: timings.cas_put.as_millis(),
                db_apply_ms: timings.db_apply.as_millis(),
            },
            regions: timings
                .regions
                .iter()
                .map(|region| RegionTimingJson {
                    path: region.location.clone(),
                    bytes: region.bytes,
                    chunks: region.chunks,
                    open_ms: region.open.as_millis(),
                    ingest_ms: region.ingest.as_millis(),
                    hash_ms: region.hash.as_millis(),
                    cas_ms: region.cas.as_millis(),
                })
                .collect(),
        }),
    }
}

fn print_timing_table(timings: &CommitTimings, style: Styler) {
    println!(
        "{}",
        style.dim(&format!(
            "timing total={}ms discover={}ms universe={}ms fp={}ms open={}ms ingest={}ms (hash={}ms cas={}ms) db={}ms ingested_files={} skipped_files={} carried_chunks={}",
            timings.total.as_millis(),
            timings.discover.as_millis(),
            timings.universe_load.as_millis(),
            timings.fingerprint.as_millis(),
            timings.region_open.as_millis(),
            timings.ingest.as_millis(),
            timings.hash.as_millis(),
            timings.cas_put.as_millis(),
            timings.db_apply.as_millis(),
            timings.regions.len(),
            timings.skipped_regions,
            timings.carried_chunks,
        ))
    );
    let mut slowest: Vec<&sekai_app::RegionTiming> = timings.regions.iter().collect();
    slowest.sort_by_key(|r| std::cmp::Reverse((r.open + r.ingest).as_micros()));
    for region in slowest.iter().take(5) {
        println!(
            "{}",
            style.dim(&format!(
                "  {} chunks={} bytes={} open={}ms ingest={}ms (hash={}ms cas={}ms)",
                region.location,
                region.chunks,
                region.bytes,
                region.open.as_millis(),
                region.ingest.as_millis(),
                region.hash.as_millis(),
                region.cas.as_millis(),
            ))
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::time::Duration;

    #[test]
    fn renders_commit_payload() {
        let report = CommitReport {
            commit: sekai_app::CommitId([3; 32]),
            chunks: 40,
            new_blobs: 2,
            tombstones: 0,
            skipped_regions: 1,
            carried_chunks: 8,
        };
        let timings = CommitTimings {
            total: Duration::from_millis(100),
            discover: Duration::from_millis(1),
            universe_load: Duration::from_millis(2),
            fingerprint: Duration::from_millis(3),
            region_open: Duration::from_millis(4),
            ingest: Duration::from_millis(50),
            hash: Duration::from_millis(6),
            cas_put: Duration::from_millis(7),
            db_apply: Duration::from_millis(8),
            skipped_regions: 1,
            carried_chunks: 8,
            regions: vec![sekai_app::RegionTiming {
                location: "region/r.0.0.mca".to_owned(),
                bytes: 100,
                chunks: 32,
                open: Duration::from_millis(4),
                ingest: Duration::from_millis(5),
                hash: Duration::from_millis(6),
                cas: Duration::from_millis(7),
            }],
        };
        let id = report.commit;
        let payload = commit_payload(&report, &timings, false);
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            format!(
                r#"{{"commit":"{id}","chunks":40,"new_blobs":2,"tombstones":0,"skipped_regions":1,"carried_chunks":8}}"#
            )
        );
        let payload_timed = commit_payload(&report, &timings, true);
        assert_eq!(
            serde_json::to_string(&payload_timed).unwrap(),
            format!(
                r#"{{"commit":"{id}","chunks":40,"new_blobs":2,"tombstones":0,"skipped_regions":1,"carried_chunks":8,"total_ms":100,"phases":{{"discover_ms":1,"universe_load_ms":2,"fingerprint_ms":3,"region_open_ms":4,"ingest_ms":50,"hash_ms":6,"cas_put_ms":7,"db_apply_ms":8}},"regions":[{{"path":"region/r.0.0.mca","bytes":100,"chunks":32,"open_ms":4,"ingest_ms":5,"hash_ms":6,"cas_ms":7}}]}}"#
            )
        );
    }

    #[test]
    fn detects_preexisting_stores() {
        assert!(store_preexists(std::env::temp_dir().to_str().unwrap()));
        assert!(!store_preexists(
            std::env::temp_dir()
                .join("sekai-missing-store-9d3f2b1a")
                .to_str()
                .unwrap()
        ));
        assert!(!store_preexists("sqlite:///definitely/missing/store"));
        assert!(store_preexists("mysql://user@localhost/store"));
    }
}
