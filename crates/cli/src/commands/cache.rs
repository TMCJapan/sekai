//! Cache subcommand execution, DTOs, and output rendering.

use anyhow::Context as _;
use sekai_app::{CacheUpdateOptions, CacheUpdateReport, CacheUpdateTimings};
use sekai_storage_local::LocalStore;
use sekai_world_local::HostWorldTree;
use serde::Serialize;

use super::progress::Bars;
use crate::cli::CacheCommand;
use crate::envelope::envelope_ok;
use crate::style::Styler;

pub async fn run(store: &str, action: &CacheCommand, style: Styler) -> anyhow::Result<()> {
    match action {
        CacheCommand::Clean { json } => {
            let mut instance = sekai_app::SekaiInstance::<LocalStore>::open(store).await?;
            let entries = instance
                .clean_cache()
                .await
                .with_context(|| format!("cache clean for {store} failed"))?;
            if *json {
                println!("{}", envelope_ok("cache", &CacheClean { entries })?);
                return Ok(());
            }
            println!(
                "cleared {} cached region image(s)",
                style.bold(&entries.to_string())
            );
        }
        CacheCommand::Update {
            world,
            output,
            jobs,
        } => {
            let mut instance = sekai_app::SekaiInstance::<LocalStore, _, _>::open_with(
                store,
                <crate::runner::OsThread as sekai_app::ThreadRunner>::new(*jobs),
                crate::time::OsTime,
            )
            .await?;
            let mut world_tree = HostWorldTree::new(world)
                .with_context(|| format!("failed to inspect world {}", world.display()))?;
            let bars = Bars::new(output.progress);
            let options = CacheUpdateOptions::default();
            let (report, timings) = instance
                .world_mut(&mut world_tree)
                .update_cache(options, &bars)
                .await
                .with_context(|| format!("cache update of {} failed", world.display()))?;
            if output.json {
                println!(
                    "{}",
                    envelope_ok("cache", &update_payload(&report, &timings, output.timing))?
                );
                return Ok(());
            }
            println!("{}", update_line(&report, style));
            if output.timing {
                print_update_timing_table(&timings, style);
            }
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct CacheClean {
    entries: u64,
}

fn update_line(report: &CacheUpdateReport, style: Styler) -> String {
    format!(
        "cached {} new region image(s), {} already present, {} empty ({} files, {} chunks)",
        style.bold(&report.entries.to_string()),
        report.existing,
        report.empty,
        report.files,
        report.chunks,
    )
}

#[derive(Serialize)]
struct CacheUpdate {
    files: usize,
    chunks: usize,
    entries: usize,
    existing: usize,
    empty: usize,
    #[serde(flatten)]
    timing: Option<CacheUpdateTiming>,
}

#[derive(Serialize)]
struct CacheUpdateTiming {
    total_ms: u128,
    phases: CacheUpdatePhases,
}

#[allow(clippy::struct_field_names)]
#[derive(Serialize)]
struct CacheUpdatePhases {
    discover_ms: u128,
    ingest_ms: u128,
    hash_ms: u128,
    cache_ms: u128,
}

fn update_payload(
    report: &CacheUpdateReport,
    timings: &CacheUpdateTimings,
    timing: bool,
) -> CacheUpdate {
    CacheUpdate {
        files: report.files,
        chunks: report.chunks,
        entries: report.entries,
        existing: report.existing,
        empty: report.empty,
        timing: timing.then_some(CacheUpdateTiming {
            total_ms: timings.total.as_millis(),
            phases: CacheUpdatePhases {
                discover_ms: timings.discover.as_millis(),
                ingest_ms: timings.ingest.as_millis(),
                hash_ms: timings.hash.as_millis(),
                cache_ms: timings.cache.as_millis(),
            },
        }),
    }
}

fn print_update_timing_table(timings: &CacheUpdateTimings, style: Styler) {
    println!(
        "{}",
        style.dim(&format!(
            "timing total={}ms discover={}ms ingest={}ms (hash={}ms) cache={}ms",
            timings.total.as_millis(),
            timings.discover.as_millis(),
            timings.ingest.as_millis(),
            timings.hash.as_millis(),
            timings.cache.as_millis(),
        ))
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::time::Duration;

    #[test]
    fn renders_cache_clean_payload() {
        assert_eq!(
            serde_json::to_string(&CacheClean { entries: 3 }).unwrap(),
            r#"{"entries":3}"#
        );
    }

    fn report() -> CacheUpdateReport {
        CacheUpdateReport {
            files: 4,
            chunks: 101,
            entries: 3,
            existing: 1,
            empty: 0,
        }
    }

    fn timings() -> CacheUpdateTimings {
        CacheUpdateTimings {
            total: Duration::from_millis(120),
            discover: Duration::from_millis(1),
            ingest: Duration::from_millis(80),
            hash: Duration::from_millis(30),
            cache: Duration::from_millis(20),
        }
    }

    #[test]
    fn renders_cache_update_payload() {
        let payload = update_payload(&report(), &timings(), false);
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            r#"{"files":4,"chunks":101,"entries":3,"existing":1,"empty":0}"#
        );
        let payload_timed = update_payload(&report(), &timings(), true);
        assert_eq!(
            serde_json::to_string(&payload_timed).unwrap(),
            r#"{"files":4,"chunks":101,"entries":3,"existing":1,"empty":0,"total_ms":120,"phases":{"discover_ms":1,"ingest_ms":80,"hash_ms":30,"cache_ms":20}}"#
        );
    }

    #[test]
    fn renders_cache_update_line() {
        assert_eq!(
            update_line(&report(), Styler::disabled()),
            "cached 3 new region image(s), 1 already present, 0 empty (4 files, 101 chunks)"
        );
    }
}
