//! Restore subcommand execution and output rendering.
//!
//! Restore reports with checkout's payload and timing phases: the run is
//! a scoped checkout under git-restore defaults (ADR-0039).

use anyhow::Context as _;
use sekai_app::{HostWorldTree, RestoreOptions, Revision};
use sekai_storage_local::LocalStore;

use super::checkout::{checkout_payload, print_checkout_timing_table};
use super::progress::Bars;
use super::{ReportOut, resolve_selection};
use crate::cli::{OnMissingBlob, OnMissingFile, RestoreArgs};
use crate::envelope::envelope_ok;
use crate::style::Styler;

const fn map_options(args: &RestoreArgs) -> RestoreOptions {
    RestoreOptions {
        overlay: args.overlay,
        on_missing_blob: match args.on_missing_blob {
            OnMissingBlob::Abort => sekai_app::MissingBlobPolicy::Abort,
            OnMissingBlob::SkipChunk => sekai_app::MissingBlobPolicy::SkipChunk,
        },
        on_missing_file: match args.on_missing_file {
            OnMissingFile::SiblingFirst => sekai_app::MissingFilePolicy::SiblingFirst,
            OnMissingFile::DerivedOnly => sekai_app::MissingFilePolicy::DerivedOnly,
            OnMissingFile::Error => sekai_app::MissingFilePolicy::Error,
        },
    }
}

pub async fn run(store: &str, args: &RestoreArgs, style: Styler) -> anyhow::Result<()> {
    // git restore refuses an empty pathspec; validate before opening
    // anything so the refusal costs no store or world access.
    if !args.has_paths() {
        anyhow::bail!("you must specify path(s) to restore (--in, --region, or --scope)");
    }
    let out = ReportOut::of(args.output, style);
    let mut world = HostWorldTree::new(&args.world)
        .with_context(|| format!("failed to inspect world {}", args.world.display()))?;
    let mut instance = sekai_app::SekaiInstance::<LocalStore, _>::open_with(
        store,
        <crate::runner::OsThread as sekai_app::ThreadRunner>::new(args.jobs),
    )
    .await
    .with_context(|| format!("restore of {} failed", args.world.display()))?;
    let scope = resolve_selection(&instance, &args.selection).await?;
    let options = map_options(args);
    let source = args.source.as_ref().map_or("HEAD", Revision::as_str);
    if !out.json {
        eprintln!(
            "restore {} from {} (scope: {}, policy: {})",
            args.world.display(),
            source,
            args.selection.describe(),
            policy_summary(args),
        );
    }
    let bars = Bars::new(out.progress);
    let (report, timings) = instance
        .restore(&mut world, args.source.as_ref(), scope, options, &bars)
        .await
        .with_context(|| format!("restore of {} from {source} failed", args.world.display()))?;
    if out.json {
        println!(
            "{}",
            envelope_ok("restore", &checkout_payload(&report, &timings, out.timing))?
        );
        return Ok(());
    }
    println!(
        "restored from {}: {} files rewritten, {} files deleted, {} chunks restored",
        style.bold(source),
        report.files_written,
        report.files_deleted,
        report.chunks_restored
    );
    if out.timing {
        print_checkout_timing_table(&timings, style);
    }
    Ok(())
}

/// One-line human summary of the restore policy for the pre-run echo.
fn policy_summary(args: &RestoreArgs) -> String {
    let mut parts = Vec::new();
    if args.overlay {
        parts.push("overlay");
    }
    if matches!(args.on_missing_blob, OnMissingBlob::SkipChunk) {
        parts.push("skip missing blobs");
    }
    if !matches!(args.on_missing_file, OnMissingFile::SiblingFirst) {
        parts.push(match args.on_missing_file {
            OnMissingFile::SiblingFirst => "sibling-first",
            OnMissingFile::DerivedOnly => "derived-only",
            OnMissingFile::Error => "missing-file error",
        });
    }
    if parts.is_empty() {
        return "no-overlay".to_owned();
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    fn args(extra: &[&str]) -> RestoreArgs {
        let mut argv = vec!["sekai", "restore", "w", "--in", "overworld:0,0"];
        argv.extend_from_slice(extra);
        let cli = crate::cli::Cli::try_parse_from(argv).expect("restore parses");
        match cli.command {
            crate::cli::Command::Restore(args) => args,
            other => panic!("expected restore, got {other:?}"),
        }
    }

    #[test]
    fn maps_strategy_flags_to_options() {
        assert_eq!(map_options(&args(&[])), RestoreOptions::default());

        let lenient = args(&[
            "--overlay",
            "--on-missing-blob",
            "skip-chunk",
            "--on-missing-file",
            "error",
            "--jobs",
            "3",
        ]);
        assert_eq!(
            map_options(&lenient),
            RestoreOptions {
                overlay: true,
                on_missing_blob: sekai_app::MissingBlobPolicy::SkipChunk,
                on_missing_file: sekai_app::MissingFilePolicy::Error,
            }
        );
    }

    #[test]
    fn summarizes_restore_policy() {
        assert_eq!(policy_summary(&args(&[])), "no-overlay");
        let lenient = args(&[
            "--overlay",
            "--on-missing-blob",
            "skip-chunk",
            "--on-missing-file",
            "derived-only",
        ]);
        assert_eq!(
            policy_summary(&lenient),
            "overlay, skip missing blobs, derived-only"
        );
    }

    #[test]
    fn demands_a_pathspec() {
        assert!(args(&[]).has_paths());
        assert!(args(&["--region", "overworld:1,0"]).has_paths());

        let mut argv = vec!["sekai", "restore", "w"];
        argv.extend_from_slice(&["--kind", "region"]);
        let cli = crate::cli::Cli::try_parse_from(argv).expect("restore parses");
        let crate::cli::Command::Restore(bare) = cli.command else {
            panic!("expected restore");
        };
        // `--kind` alone narrows families but spells no path.
        assert!(!bare.has_paths());
    }
}
