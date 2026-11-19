//! Log subcommand execution, DTOs, and output rendering (ADR-0036 *log*).

use std::io::IsTerminal as _;

use sekai_app::{CommitStats, Decoration, Location, LogEntry, LogOptions, RefKind};
use serde::Serialize;

use crate::cli::LogArgs;
use crate::envelope::Printer;
use crate::style::Styler;

pub async fn run(location: &Location, args: &LogArgs, printer: &Printer) -> anyhow::Result<()> {
    let style = printer.style;
    let instance = sekai_app::SekaiInstance::open_location(location).await?;
    // Decorations show on a terminal unless an option says otherwise, as
    // Git's `log.decorate=auto` does. JSON output reads them too.
    let decorate = if args.decorate || args.no_decorate {
        args.decorate
    } else {
        std::io::stdout().is_terminal()
    };
    let options = LogOptions {
        all: args.all,
        reverse: args.reverse,
        decorate: decorate || args.json,
    };
    let entries = instance.log(&args.ranges, options).await?;
    let mut stats = Vec::new();
    if args.stat {
        for entry in &entries {
            stats.push(instance.commit_stats(entry.commit.id).await?);
        }
    }
    if args.json {
        printer.print_ok(
            "log",
            &log_payload(&entries, args.stat.then_some(stats.as_slice())),
        )?;
        return Ok(());
    }
    for (index, entry) in entries.iter().enumerate() {
        let decorations = if decorate {
            decorations(&entry.decorations, style)
        } else {
            String::new()
        };
        let stat = stats.get(index).map(stat_line);
        if args.oneline {
            println!(
                "{}{decorations}",
                style.yellow(&entry.commit.id.to_string())
            );
            if let Some(stat) = stat {
                println!(" {stat}");
            }
            continue;
        }
        if index > 0 {
            println!();
        }
        println!(
            "{}{decorations}",
            style.yellow(&format!("commit {}", entry.commit.id))
        );
        println!("Date:   {}", git_date(entry.commit.created_at_ms));
        if let Some(stat) = stat {
            println!("\n {stat}");
        }
    }
    Ok(())
}

/// Git's `--decorate=short` suffix, such as ` (HEAD -> main, tag: v1)`,
/// or nothing for an undecorated commit.
fn decorations(decorations: &[Decoration], style: Styler) -> String {
    if decorations.is_empty() {
        return String::new();
    }
    let names: Vec<String> = decorations
        .iter()
        .map(|decoration| match decoration {
            Decoration::Head(Some(branch)) => {
                format!("{} -> {}", style.cyan("HEAD"), style.green(branch.as_str()))
            }
            Decoration::Head(None) => style.cyan("HEAD"),
            Decoration::Ref(r) => match r.kind {
                RefKind::Branch => style.green(r.name.as_str()),
                RefKind::Tag => style.yellow(&format!("tag: {}", r.name)),
            },
        })
        .collect();
    format!(" ({})", names.join(", "))
}

/// Unix milliseconds in Git's default date format, in local time, such
/// as `Wed Nov 18 12:41:36 2026 +0900`; raw millis when out of range.
fn git_date(created_at_ms: u64) -> String {
    i64::try_from(created_at_ms)
        .ok()
        .and_then(|millis| jiff::Timestamp::from_millisecond(millis).ok())
        .map_or_else(
            || format!("{created_at_ms}ms"),
            |time| {
                time.to_zoned_datetime(jiff::tz::TimeZone::system())
                    .strftime("%a %b %-d %H:%M:%S %Y %z")
                    .to_string()
            },
        )
}

fn stat_line(stats: &CommitStats) -> String {
    format!(
        "fresh={} tombstones={} new_blobs={} effective={}",
        stats.fresh_chunks, stats.fresh_tombstones, stats.new_blobs, stats.effective_chunks
    )
}

#[derive(Serialize)]
struct CommitJson {
    id: String,
    created_at_ms: u64,
    /// Full refnames that point at the commit.
    refs: Vec<String>,
    /// Whether HEAD stands at the commit.
    head: bool,
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

fn log_payload(entries: &[LogEntry], stats: Option<&[CommitStats]>) -> Vec<CommitJson> {
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| CommitJson {
            id: entry.commit.id.to_string(),
            created_at_ms: entry.commit.created_at_ms,
            refs: entry
                .decorations
                .iter()
                .filter_map(|decoration| match decoration {
                    Decoration::Head(branch) => branch
                        .as_ref()
                        .map(|branch| RefKind::Branch.full_name(branch)),
                    Decoration::Ref(r) => Some(r.full_name()),
                })
                .collect(),
            head: entry
                .decorations
                .iter()
                .any(|decoration| matches!(decoration, Decoration::Head(_))),
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
    use sekai_app::{Commit, CommitId, Ref, RefName, RootHash};

    fn name(raw: &str) -> RefName {
        RefName::parse(raw).unwrap()
    }

    fn entry(id: CommitId, decorations: Vec<Decoration>) -> LogEntry {
        LogEntry {
            commit: Commit {
                id,
                created_at_ms: 1_700_000_000_000,
                parents: vec![],
                root: RootHash::new([1; 32]),
            },
            decorations,
        }
    }

    #[test]
    fn decorates_as_git_short_does() {
        let style = Styler::disabled();
        assert_eq!(decorations(&[], style), "");
        assert_eq!(
            decorations(
                &[
                    Decoration::Head(Some(name("main"))),
                    Decoration::Ref(Ref::tag(name("v1"))),
                    Decoration::Ref(Ref::branch(name("dev"))),
                ],
                style
            ),
            " (HEAD -> main, tag: v1, dev)"
        );
        assert_eq!(decorations(&[Decoration::Head(None)], style), " (HEAD)");
    }

    #[test]
    fn dates_out_of_range_fall_back_to_millis() {
        assert_eq!(git_date(u64::MAX), format!("{}ms", u64::MAX));
    }

    #[test]
    fn renders_log_json() {
        let first = CommitId([1; 32]);
        let entries = vec![
            entry(
                first,
                vec![
                    Decoration::Head(Some(name("main"))),
                    Decoration::Ref(Ref::tag(name("v1"))),
                ],
            ),
            entry(CommitId([2; 32]), vec![]),
        ];
        let json = serde_json::to_string(&log_payload(&entries, None)).unwrap();
        assert!(json.starts_with(&format!(
            r#"[{{"id":"{first}","created_at_ms":1700000000000,"refs":["refs/heads/main","refs/tags/v1"],"head":true}}"#
        )));
        assert!(json.ends_with(r#""refs":[],"head":false}]"#));
        let stats = vec![CommitStats {
            fresh_chunks: 2,
            fresh_tombstones: 0,
            new_blobs: 1,
            effective_chunks: 2,
        }];
        let json = serde_json::to_string(&log_payload(&entries[..1], Some(&stats))).unwrap();
        assert!(json.ends_with(
            r#""head":true,"fresh_chunks":2,"fresh_tombstones":0,"new_blobs":1,"effective_chunks":2}]"#
        ));
    }
}
