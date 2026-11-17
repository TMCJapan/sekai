//! Commit subcommand execution, DTOs, and output rendering.
//!
//! `sekai commit` is two sequential app operations (ADR-0034): the
//! commit-free cache update warms the region-image cache from the live
//! world, then the commit records history. The `cache_ms` phase reports
//! the update pass; `total_ms` spans both. `--no-cache` skips the update
//! pass, leaving `cache_ms` at zero and the cache untouched.

use anyhow::Context as _;
use sekai_app::{
    CacheUpdateOptions, CacheUpdateTimings, CommitOptions, CommitReport, CommitTimings,
    HostWorldTree,
};
use serde::Serialize;
use std::path::Path;

use super::progress::Bars;
use super::{ReportOut, resolve_selection};
use crate::cli::Selection;
use crate::envelope::envelope_ok;
use crate::style::Styler;

const fn options(jobs: usize) -> CommitOptions {
    CommitOptions { concurrency: jobs }
}

pub async fn run(
    store: &str,
    world_path: impl AsRef<Path>,
    jobs: usize,
    no_cache: bool,
    selection: &Selection,
    out: ReportOut,
) -> anyhow::Result<()> {
    let mut world = HostWorldTree::new(&world_path)
        .with_context(|| format!("failed to inspect world {}", world_path.as_ref().display()))?;
    let mut instance = sekai_app::SekaiInstance::open(store)
        .await
        .with_context(|| format!("commit of {} failed", world_path.as_ref().display()))?;
    let scope = resolve_selection(&instance, selection).await?;
    let bars = Bars::new(out.progress);
    // Cache update first, record second: the update keys each file under
    // the region node its own chunks produce, so a stable world leaves the
    // commit naming exactly the hashes just cached (ADR-0034). `--no-cache`
    // skips the pass; zero timings keep `cache_ms` at 0 and `total_ms`
    // spanning the commit alone, so the payload shape is unchanged.
    let update_timings = if no_cache {
        CacheUpdateTimings::default()
    } else {
        let (_, update_timings) = instance
            .world_mut(&mut world)
            .update_cache(CacheUpdateOptions { concurrency: jobs }, &bars)
            .await
            .with_context(|| {
                format!(
                    "cache update before commit of {} failed",
                    world_path.as_ref().display()
                )
            })?;
        update_timings
    };
    let (report, timings) = instance
        .world_mut(&mut world)
        .commit(options(jobs), scope, &bars)
        .await
        .with_context(|| format!("commit of {} failed", world_path.as_ref().display()))?;
    if out.json {
        println!(
            "{}",
            envelope_ok(
                "commit",
                &commit_payload(&report, &timings, &update_timings, out.timing)
            )?
        );
        return Ok(());
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
        print_timing_table(&timings, &update_timings, style);
    }
    Ok(())
}

#[derive(Serialize)]
struct CommitPayload {
    commit: String,
    chunks: usize,
    new_blobs: usize,
    tombstones: usize,
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
    region_open_ms: u128,
    ingest_ms: u128,
    hash_ms: u128,
    cas_put_ms: u128,
    db_apply_ms: u128,
    /// The cache update pass run before recording (ADR-0034).
    cache_ms: u128,
}

/// One region row of the `--timing` JSON (`docs/json.md`).
///
/// Field names and types are the machine-readable contract: a consumer such
/// as a benchmark script reads them, so changing or dropping one is an
/// output-contract change that updates `docs/json.md` and the golden test
/// in this module together.
#[derive(Serialize)]
struct RegionTimingJson {
    path: String,
    chunks: usize,
    open_ms: u128,
    ingest_ms: u128,
    hash_ms: u128,
    cas_ms: u128,
}

fn commit_payload(
    report: &CommitReport,
    timings: &CommitTimings,
    update_timings: &CacheUpdateTimings,
    timing: bool,
) -> CommitPayload {
    CommitPayload {
        commit: report.commit.to_string(),
        chunks: report.chunks,
        new_blobs: report.new_blobs,
        tombstones: report.tombstones,
        // `then`, not `then_some`: `then` evaluates its closure lazily,
        // while `then_some` eagerly builds the timing block (and a copy of
        // every region's location) even without `--timing`.
        timing: timing.then(|| CommitTiming {
            total_ms: (timings.total + update_timings.total).as_millis(),
            phases: CommitPhases {
                discover_ms: timings.discover.as_millis(),
                universe_load_ms: timings.universe_load.as_millis(),
                region_open_ms: timings.region_open.as_millis(),
                ingest_ms: timings.ingest.as_millis(),
                hash_ms: timings.hash.as_millis(),
                cas_put_ms: timings.cas_put.as_millis(),
                db_apply_ms: timings.db_apply.as_millis(),
                cache_ms: update_timings.total.as_millis(),
            },
            regions: timings
                .regions
                .iter()
                .map(|region| RegionTimingJson {
                    path: region.location.clone(),
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

fn print_timing_table(timings: &CommitTimings, update_timings: &CacheUpdateTimings, style: Styler) {
    println!(
        "{}",
        style.dim(&format!(
            "timing total={}ms discover={}ms universe={}ms open={}ms ingest={}ms (hash={}ms cas={}ms) db={}ms cache={}ms ingested_files={}",
            (timings.total + update_timings.total).as_millis(),
            timings.discover.as_millis(),
            timings.universe_load.as_millis(),
            timings.region_open.as_millis(),
            timings.ingest.as_millis(),
            timings.hash.as_millis(),
            timings.cas_put.as_millis(),
            timings.db_apply.as_millis(),
            update_timings.total.as_millis(),
            timings.regions.len(),
        ))
    );
    let mut slowest: Vec<&sekai_app::RegionTiming> = timings.regions.iter().collect();
    slowest.sort_by_key(|r| std::cmp::Reverse((r.open + r.ingest).as_micros()));
    for region in slowest.iter().take(5) {
        println!(
            "{}",
            style.dim(&format!(
                "  {} chunks={} open={}ms ingest={}ms (hash={}ms cas={}ms)",
                region.location,
                region.chunks,
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
        };
        let timings = CommitTimings {
            total: Duration::from_millis(100),
            discover: Duration::from_millis(1),
            universe_load: Duration::from_millis(2),
            region_open: Duration::from_millis(4),
            ingest: Duration::from_millis(50),
            hash: Duration::from_millis(6),
            cas_put: Duration::from_millis(7),
            db_apply: Duration::from_millis(8),
            regions: vec![sekai_app::RegionTiming {
                location: "region/r.0.0.mca".to_owned(),
                chunks: 32,
                open: Duration::from_millis(4),
                ingest: Duration::from_millis(5),
                hash: Duration::from_millis(6),
                cas: Duration::from_millis(7),
            }],
        };
        let update_timings = CacheUpdateTimings {
            total: Duration::from_millis(120),
            discover: Duration::from_millis(1),
            ingest: Duration::from_millis(80),
            hash: Duration::from_millis(30),
            cache: Duration::from_millis(20),
        };
        let id = report.commit;
        let payload = commit_payload(&report, &timings, &update_timings, false);
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            format!(r#"{{"commit":"{id}","chunks":40,"new_blobs":2,"tombstones":0}}"#)
        );
        let payload_timed = commit_payload(&report, &timings, &update_timings, true);
        assert_eq!(
            serde_json::to_string(&payload_timed).unwrap(),
            format!(
                r#"{{"commit":"{id}","chunks":40,"new_blobs":2,"tombstones":0,"total_ms":220,"phases":{{"discover_ms":1,"universe_load_ms":2,"region_open_ms":4,"ingest_ms":50,"hash_ms":6,"cas_put_ms":7,"db_apply_ms":8,"cache_ms":120}},"regions":[{{"path":"region/r.0.0.mca","chunks":32,"open_ms":4,"ingest_ms":5,"hash_ms":6,"cas_ms":7}}]}}"#
            )
        );
    }
}
