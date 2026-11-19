use clap::Parser as _;

const H1: &str = "0101010101010101010101010101010101010101010101010101010101010101";
const H2: &str = "0202020202020202020202020202020202020202020202020202020202020202";
const H3: &str = "0303030303030303030303030303030303030303030303030303030303030303";
use sekai_app::{Dimension, Rect, Revision};
use sekai_cli::{
    cli::{
        BranchAction, CacheCommand, CheckoutArgs, Cli, Command, DiffArgs, DiffTarget,
        InlineSelection, LogArgs, OnMissingBlob, OnMissingFile, RegionSpec, ScopeCommand,
        Selection, TagAction, TimingArgs, WorktreeCommand,
    },
    style::ColorChoice,
};

/// The inline flags of a selection, or a panic for a named scope.
fn inline(selection: &Selection) -> &InlineSelection {
    match selection {
        Selection::Inline(inline) => inline,
        Selection::Scope(name) => panic!("expected an inline selection, got scope {name}"),
    }
}

/// `Option<Revision>` as `Option<&str>` for assertions.
fn ref_str(value: Option<&Revision>) -> Option<&str> {
    value.map(Revision::as_str)
}

/// The checkout arguments of a parsed invocation.
fn checkout(args: &[&str]) -> CheckoutArgs {
    match Cli::try_parse_from(args).expect("checkout parses").command {
        Command::Checkout(args) => args,
        other => panic!("expected checkout, got {other:?}"),
    }
}

#[test]
fn parses_subcommands() {
    let cli = Cli::try_parse_from(["sekai", "commit"]).expect("commit parses");
    assert!(matches!(cli.command, Command::Commit { .. }));
    assert_eq!((cli.world, cli.store), (None, None));

    // `--world` and `--store` are global: they go before or after the
    // command, and no command takes a world of its own (ADR-0036).
    for args in [
        vec!["sekai", "--world", "w", "--store", "s", "status"],
        vec!["sekai", "status", "--world", "w", "--store", "s"],
    ] {
        let cli = Cli::try_parse_from(&args).unwrap_or_else(|err| panic!("{args:?}: {err}"));
        assert_eq!(cli.world, Some("w".into()));
        assert_eq!(cli.store, Some("s".into()));
    }
    for args in [
        vec!["sekai", "commit", "w"],
        vec!["sekai", "status", "w"],
        vec!["sekai", "checkout", "w", H3],
    ] {
        assert!(
            Cli::try_parse_from(&args).is_err(),
            "{args:?} must refuse a positional world"
        );
    }

    for target in [
        H3,
        "main",
        "feature/nightly",
        "HEAD",
        "@",
        "main~2",
        "v1~1",
        "main",
    ] {
        let args = checkout(&["sekai", "checkout", target]);
        assert_eq!(ref_str(args.target.as_ref()), Some(target));
    }
    // Without a target, checkout acts on HEAD (ADR-0036 *checkout*).
    let args = checkout(&["sekai", "checkout", "-f"]);
    assert!(args.target.is_none() && args.force && !args.selection.is_given());
    let args = checkout(&["sekai", "checkout", "--in", "overworld"]);
    assert!(args.target.is_none() && args.selection.is_given());
    assert_eq!(checkout(&["sekai", "checkout", H3]).jobs, 0);
    assert_eq!(checkout(&["sekai", "checkout", H3, "--jobs", "4"]).jobs, 4);

    let cli = Cli::try_parse_from(["sekai", "status"]).expect("status parses");
    assert!(matches!(
        cli.command,
        Command::Status {
            jobs: 0,
            output: TimingArgs {
                timing: false,
                json: false,
                ..
            },
            ..
        }
    ));
    assert_eq!(cli.command.name(), "status");
    assert!(!cli.command.output_json());

    let cli = Cli::try_parse_from(["sekai", "status", "--timing", "--json", "--jobs", "4"])
        .expect("status options parse");
    assert!(matches!(
        cli.command,
        Command::Status {
            jobs: 4,
            output: TimingArgs {
                timing: true,
                json: true,
                ..
            },
            ..
        }
    ));
    assert!(cli.command.output_json());
}

#[test]
fn parses_progress_flags_as_git_does() {
    let progress = |args: &[&str]| match Cli::try_parse_from(args).expect("parses").command {
        Command::Commit { output, .. } => output.progress.show(output.json),
        other => panic!("expected commit, got {other:?}"),
    };
    // On by default, off with `--no-progress` or `--json`; the last of
    // `--progress` and `--no-progress` wins (ADR-0036 *Options*).
    assert!(progress(&["sekai", "commit"]));
    assert!(progress(&["sekai", "commit", "--progress"]));
    assert!(!progress(&["sekai", "commit", "--no-progress"]));
    assert!(progress(&[
        "sekai",
        "commit",
        "--no-progress",
        "--progress"
    ]));
    assert!(!progress(&[
        "sekai",
        "commit",
        "--progress",
        "--no-progress"
    ]));
    assert!(!progress(&["sekai", "commit", "--json"]));
    assert!(!progress(&["sekai", "commit", "--json", "--no-progress"]));

    // `--progress` stays refused with `--json`, so stderr stays silent.
    for args in [
        vec!["sekai", "commit", "--progress", "--json"],
        vec!["sekai", "commit", "--json", "--progress"],
        vec!["sekai", "checkout", H3, "--progress", "--json"],
        vec!["sekai", "diff", "--progress", "--json"],
        vec!["sekai", "worktree", "add", "w", "--progress", "--json"],
    ] {
        assert!(Cli::try_parse_from(&args).is_err(), "{args:?} is refused");
    }
    for args in [
        vec!["sekai", "checkout", H3, "--no-progress"],
        vec!["sekai", "diff", "--no-progress"],
        vec!["sekai", "cache", "update", "--no-progress"],
        vec!["sekai", "worktree", "add", "w", "--no-progress"],
    ] {
        Cli::try_parse_from(&args).unwrap_or_else(|err| panic!("{args:?} parses: {err}"));
    }

    // `-j` is short for `--jobs`.
    assert_eq!(checkout(&["sekai", "checkout", H3, "-j", "3"]).jobs, 3);
    for args in [
        vec!["sekai", "commit", "-j", "2"],
        vec!["sekai", "status", "-j", "2"],
        vec!["sekai", "cache", "update", "-j", "2"],
    ] {
        Cli::try_parse_from(&args).unwrap_or_else(|err| panic!("{args:?} parses: {err}"));
    }
}

#[test]
fn parses_no_cache_flag() {
    // Default: the pre-commit cache update pass runs.
    let cli = Cli::try_parse_from(["sekai", "commit"]).expect("commit parses");
    assert!(matches!(
        cli.command,
        Command::Commit {
            no_cache: false,
            ..
        }
    ));

    let cli =
        Cli::try_parse_from(["sekai", "commit", "--no-cache"]).expect("commit --no-cache parses");
    assert!(matches!(
        cli.command,
        Command::Commit { no_cache: true, .. }
    ));

    // The flag is commit-only: other commands refuse it.
    for args in [
        vec!["sekai", "status", "--no-cache"],
        vec!["sekai", "checkout", H3, "--no-cache"],
        vec!["sekai", "cache", "update", "--no-cache"],
    ] {
        assert!(
            Cli::try_parse_from(&args).is_err(),
            "{args:?} must be refused"
        );
    }
}

#[test]
fn parses_diff_command() {
    let cli = Cli::try_parse_from(["sekai", "diff", H1, H2, "--in", "overworld:10,-5"])
        .expect("diff parses");
    assert!(matches!(
        cli.command,
        Command::Diff(DiffArgs {
            target: DiffTarget::Commits { .. },
            output: TimingArgs {
                timing: false,
                json: false,
                ..
            },
            show_values: false,
            ..
        })
    ));
    if let Command::Diff(args) = &cli.command {
        // Default kinds cover all three families.
        assert_eq!(inline(&args.selection).explicit_chunks().len(), 3);
        assert_eq!(
            args.target,
            DiffTarget::Commits {
                old: Revision::parse(H1).unwrap(),
                new: Revision::parse(H2).unwrap(),
            }
        );
    } else {
        panic!("expected diff");
    }

    // `diff` takes commit-ishes; a range is not one of them yet.
    assert!(Cli::try_parse_from(["sekai", "diff", "a...b", H2]).is_err());

    let cli = Cli::try_parse_from([
        "sekai",
        "diff",
        "--in",
        "overworld:10,-5",
        "--kind",
        "region",
    ])
    .expect("diff with the world parses");
    assert!(matches!(
        cli.command,
        Command::Diff(DiffArgs {
            target: DiffTarget::World(None),
            output: TimingArgs {
                timing: false,
                json: false,
                ..
            },
            show_values: false,
            ..
        })
    ));
    if let Command::Diff(args) = &cli.command {
        let chunks = inline(&args.selection).explicit_chunks();
        assert_eq!(chunks.len(), 1);
        assert_eq!((chunks[0].x, chunks[0].z), (10, -5));
    } else {
        panic!("expected diff");
    }

    let cli = Cli::try_parse_from([
        "sekai",
        "diff",
        H1,
        H2,
        "--in",
        "overworld:0,0",
        "--show-values",
    ])
    .expect("diff --show-values parses");
    assert!(matches!(
        cli.command,
        Command::Diff(DiffArgs {
            show_values: true,
            ..
        })
    ));

    // Repeated areas compose by union; `--region` is additive, not exclusive.
    let cli = Cli::try_parse_from([
        "sekai",
        "diff",
        "--in",
        "overworld:0,0",
        "--in",
        "nether",
        "--region",
        "overworld:1,0",
        "--kind",
        "region",
    ])
    .expect("repeated areas parse");
    if let Command::Diff(args) = &cli.command {
        let selection = inline(&args.selection);
        assert!(selection.has_broad_areas());
        assert_eq!(selection.explicit_chunks().len(), 1);
        let scope = selection.owned_scope();
        assert!(matches!(scope, sekai_app::Scope::Select { .. }));
    } else {
        panic!("expected diff");
    }

    // `tag` takes Git's mode options, and `ref` is gone (ADR-0036).
    assert!(Cli::try_parse_from(["sekai", "ref", "list"]).is_err());

    // Removed flags fail loudly.
    assert!(Cli::try_parse_from(["sekai", "diff", "--cx", "0"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "diff", "--dimension"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "commit", "--dim", "nether"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "diff", "--in", "bogus"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "diff", "--in", "overworld:bogus"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "diff", "--in", "overworld:1,2,3"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "diff", "--region", "0,0"]).is_err());
}

#[test]
fn validates_revisions_at_parse_time() {
    // Malformed revisions and names are refused before any code runs, so
    // they can never touch the store (ADR-0013).
    for args in [
        vec!["sekai", "checkout", ""],
        vec!["sekai", "checkout", "bad name"],
        vec!["sekai", "checkout", "/main"],
        vec!["sekai", "checkout", "main/"],
        vec!["sekai", "checkout", "a//b"],
        vec!["sekai", "checkout", "a/../b"],
        vec!["sekai", "checkout", "@{-1}"],
        vec!["sekai", "checkout", "main~x"],
        vec!["sekai", "diff", H1, "bad name"],
        vec!["sekai", "branch", "bad name"],
        vec!["sekai", "branch", "main", "bad name"],
        vec!["sekai", "branch", "v..1"],
        vec!["sekai", "branch", "main", H3, "4"],
        vec!["sekai", "branch", "-d"],
        vec!["sekai", "tag", "bad name"],
        vec!["sekai", "tag", "-d"],
        vec!["sekai", "log", "a...b"],
        vec!["sekai", "reflog", "bad name"],
        vec!["sekai", "reflog", "show", "main", "extra"],
    ] {
        assert!(Cli::try_parse_from(&args).is_err(), "{args:?} is refused");
    }

    for args in [
        vec!["sekai", "checkout", "v1.2_x-1"],
        vec!["sekai", "diff", H1, H2],
        vec!["sekai", "branch", "main"],
        vec!["sekai", "branch", "123"],
        vec!["sekai", "branch", "x.lock"],
        vec![
            "sekai",
            "branch",
            "-f",
            "feature/nightly",
            "main~1",
            "--json",
        ],
        vec!["sekai", "log", "main..dev", "^v1", "main~2"],
    ] {
        Cli::try_parse_from(&args).unwrap_or_else(|err| panic!("{args:?} parses: {err}"));
    }

    // No commit and one commit compare the world; two, or a range,
    // compare commits (ADR-0036 *diff*).
    let rev = |raw: &str| Revision::parse(raw).unwrap();
    for (args, target) in [
        (vec!["sekai", "diff"], DiffTarget::World(None)),
        (
            vec!["sekai", "diff", "main"],
            DiffTarget::World(Some(rev("main"))),
        ),
        (
            vec!["sekai", "diff", H1, H2],
            DiffTarget::Commits {
                old: rev(H1),
                new: rev(H2),
            },
        ),
        (
            vec!["sekai", "diff", "v1..main"],
            DiffTarget::Commits {
                old: rev("v1"),
                new: rev("main"),
            },
        ),
        (
            vec!["sekai", "diff", "v1.."],
            DiffTarget::Commits {
                old: rev("v1"),
                new: Revision::head(),
            },
        ),
    ] {
        let Command::Diff(diff) = Cli::try_parse_from(&args).expect("diff parses").command else {
            panic!("expected diff");
        };
        assert_eq!(diff.target, target, "{args:?}");
    }
    for args in [
        vec!["sekai", "diff", H1, H2, H3],
        vec!["sekai", "diff", "^v1"],
        vec!["sekai", "diff", "v1..main", "dev"],
        vec!["sekai", "diff", "a...b"],
    ] {
        assert!(Cli::try_parse_from(&args).is_err(), "{args:?} is refused");
    }

    // The refusal names what was wrong with the revision.
    let err = Cli::try_parse_from(["sekai", "checkout", "bad name"])
        .expect_err("a malformed revision is refused");
    assert!(
        err.to_string().contains("not a commit ID or ref name"),
        "the refusal names the shape: {err}"
    );
}

#[test]
fn parses_area_specs() {
    use sekai_cli::cli::{AreaSpec, DimArea, RegionSpec};

    let whole: AreaSpec = "nether".parse().expect("whole parses");
    assert_eq!(whole.dim, Dimension::NETHER);
    assert!(matches!(whole.area, DimArea::All));

    let chunk: AreaSpec = "overworld:10,-5".parse().expect("chunk parses");
    assert!(matches!(chunk.area, DimArea::Chunk(_)));

    let rect: AreaSpec = "overworld:31,5..0,-3".parse().expect("rect parses");
    if let DimArea::Rect(rect) = rect.area {
        assert_eq!((rect.x0, rect.z0, rect.x1, rect.z1), (0, -3, 31, 5));
    } else {
        panic!("expected rect");
    }

    let region: RegionSpec = "overworld:1,-1".parse().expect("region parses");
    assert_eq!(region.dim, Dimension::OVERWORLD);

    assert!("overworld:1,2,3".parse::<AreaSpec>().is_err());
    assert!("overworld:1..2".parse::<AreaSpec>().is_err());
    assert!("bogus:1,2".parse::<AreaSpec>().is_err());
    assert!("1,2".parse::<AreaSpec>().is_err());
    assert!("0,0".parse::<RegionSpec>().is_err());
}

#[test]
fn parses_selection_for_commit_and_checkout() {
    let cli =
        Cli::try_parse_from(["sekai", "commit", "--in", "nether"]).expect("commit --in parses");
    assert!(matches!(cli.command, Command::Commit { .. }));
    if let Command::Commit { selection, .. } = &cli.command {
        let selection = inline(selection);
        assert!(selection.has_broad_areas());
        assert_eq!(
            selection.explicit_chunks(),
            [] as [sekai_app::ChunkCoord; 0]
        );
    } else {
        panic!("expected commit");
    }

    let cli = Cli::try_parse_from(["sekai", "checkout", H3, "--region", "nether:1,-1"])
        .expect("checkout --region parses");
    assert!(
        matches!(&cli.command, Command::Checkout(args) if ref_str(args.target.as_ref()) == Some(H3))
    );

    // `--in` and `--region` compose; kinds are repeatable.
    assert!(
        Cli::try_parse_from([
            "sekai",
            "commit",
            "--region",
            "nether:0,0",
            "--in",
            "nether"
        ])
        .is_ok()
    );
    let cli = Cli::try_parse_from(["sekai", "commit", "--kind", "region", "--kind", "entities"])
        .expect("repeatable --kind parses");
    if let Command::Commit { selection, .. } = &cli.command {
        assert_eq!(inline(selection).kinds().len(), 2);
    } else {
        panic!("expected commit");
    }

    // Codes outside the known families are refused at parse time instead of
    // widening into an unknown kind.
    assert!(Cli::try_parse_from(["sekai", "commit", "--kind", "9"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "commit", "--kind", "blocks"]).is_err());
}

#[test]
fn selection_owned_scope_round_trips() {
    use sekai_app::Scope;

    let base = InlineSelection {
        areas: Vec::new(),
        region: Vec::new(),
        kind: Vec::new(),
    };
    assert_eq!(base.owned_scope(), Scope::World);
    // `--kind` on its own narrows the whole world instead of being dropped.
    let kinds = InlineSelection {
        kind: vec![sekai_app::RegionKind::Region],
        ..base.clone()
    };
    let scope = kinds.owned_scope();
    assert!(matches!(scope, Scope::Kinds(ref k) if k == &[sekai_app::RegionKind::Region]));
    assert!(scope.contains(sekai_app::ChunkCoord::new(
        Dimension::NETHER,
        sekai_app::RegionKind::Region,
        4,
        4
    )));
    assert!(!scope.contains(sekai_app::ChunkCoord::new(
        Dimension::NETHER,
        sekai_app::RegionKind::Poi,
        4,
        4
    )));
    let dim = InlineSelection {
        areas: vec!["nether".parse().unwrap()],
        ..base.clone()
    };
    let scope = dim.owned_scope();
    assert!(matches!(scope, Scope::Select { .. }));
    let chunks = InlineSelection {
        areas: vec!["overworld:1,-2".parse().unwrap()],
        kind: vec![sekai_app::RegionKind::Region],
        ..base
    };
    let scope = chunks.owned_scope();
    let coord =
        sekai_app::ChunkCoord::new(Dimension::OVERWORLD, sekai_app::RegionKind::Region, 1, -2);
    assert!(scope.contains(coord));
    assert!(!scope.contains(sekai_app::ChunkCoord::new(
        Dimension::OVERWORLD,
        sekai_app::RegionKind::Region,
        2,
        -2
    )));
}

#[test]
fn selection_describe_summarizes() {
    let base = Selection::Inline(InlineSelection {
        areas: Vec::new(),
        region: Vec::new(),
        kind: Vec::new(),
    });
    assert_eq!(base.describe(), "whole world");

    let scoped = Selection::Inline(InlineSelection {
        areas: vec!["nether".parse().unwrap(), "overworld:1,-2".parse().unwrap()],
        region: vec!["overworld:0,0".parse().unwrap()],
        kind: vec![sekai_app::RegionKind::Region, sekai_app::RegionKind::Poi],
    });
    assert_eq!(
        scoped.describe(),
        "2 area(s), 1 region(s), kinds region,poi"
    );

    let named = Selection::Scope("hub/nether".parse().unwrap());
    assert_eq!(named.describe(), "hub/nether");
}

#[test]
fn parses_timing_and_json_flags() {
    let cli = Cli::try_parse_from(["sekai", "commit", "--timing"]).expect("commit --timing parses");
    assert!(matches!(
        cli.command,
        Command::Commit {
            output: TimingArgs { timing: true, .. },
            ..
        }
    ));

    let cli = Cli::try_parse_from(["sekai", "commit", "--timing", "--json"])
        .expect("commit --timing --json parses");
    assert!(matches!(
        cli.command,
        Command::Commit {
            output: TimingArgs {
                timing: true,
                json: true,
                ..
            },
            ..
        }
    ));

    assert!(
        checkout(&["sekai", "checkout", "--timing", H3])
            .output
            .timing
    );
    let cli = Cli::try_parse_from(["sekai", "checkout", "--json", H3]).expect("parses");
    assert!(cli.command.output_json());

    let args = checkout(&["sekai", "checkout", H3]);
    assert!(
        !args.keep_post_commit_files
            && !args.keep_post_commit_chunks
            && !args.keep_tombstoned_chunks
    );
    assert!(matches!(args.on_missing_blob, OnMissingBlob::Abort));
    assert!(matches!(args.on_missing_file, OnMissingFile::SiblingFirst));
    let args = checkout(&[
        "sekai",
        "checkout",
        H3,
        "--keep-post-commit-files",
        "--keep-post-commit-chunks",
        "--keep-tombstoned-chunks",
        "--on-missing-blob",
        "skip-chunk",
        "--on-missing-file",
        "error",
    ]);
    assert!(
        args.keep_post_commit_files && args.keep_post_commit_chunks && args.keep_tombstoned_chunks
    );
    assert!(matches!(args.on_missing_blob, OnMissingBlob::SkipChunk));
    assert!(matches!(args.on_missing_file, OnMissingFile::Error));
    assert!(
        Cli::try_parse_from(["sekai", "checkout", H3, "--on-missing-blob", "ignore"]).is_err(),
        "unknown missing-blob policy is refused"
    );

    let cli = Cli::try_parse_from(["sekai", "log", "--json"]).expect("log --json parses");
    assert!(matches!(
        cli.command,
        Command::Log(LogArgs { json: true, .. })
    ));
    let cli = Cli::try_parse_from(["sekai", "log", "--stat"]).expect("log --stat parses");
    assert!(matches!(
        cli.command,
        Command::Log(LogArgs {
            json: false,
            stat: true,
            ..
        })
    ));

    let cli = Cli::try_parse_from([
        "sekai",
        "diff",
        "--in",
        "overworld:0,0",
        "--timing",
        "--json",
    ])
    .expect("diff --timing --json parses");
    assert!(matches!(
        cli.command,
        Command::Diff(DiffArgs {
            output: TimingArgs {
                timing: true,
                json: true,
                ..
            },
            ..
        })
    ));
}

#[test]
fn parses_init_command() {
    let cli = Cli::try_parse_from(["sekai", "init"]).expect("init parses");
    assert!(matches!(cli.command, Command::Init { json: false }));
    assert_eq!(cli.command.name(), "init");
    assert!(!cli.command.output_json());
    assert_eq!(cli.store, None);

    let cli = Cli::try_parse_from(["sekai", "--store", "s", "init", "--json"])
        .expect("init --json parses");
    assert!(matches!(cli.command, Command::Init { json: true }));
    assert!(cli.command.output_json());
    assert_eq!(cli.store, Some("s".into()));

    // Init takes its world from `--world`, not a positional, and has no
    // phases to time.
    assert!(Cli::try_parse_from(["sekai", "init", "world"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "init", "--timing"]).is_err());
}

#[test]
fn parses_color_flag() {
    let cli = Cli::try_parse_from(["sekai", "commit"]).expect("commit parses");
    assert_eq!(cli.color, ColorChoice::Auto);

    let cli = Cli::try_parse_from(["sekai", "--color=never", "log"]).expect("parses");
    assert_eq!(cli.color, ColorChoice::Never);

    let cli = Cli::try_parse_from(["sekai", "--color", "always", "log"]).expect("parses");
    assert_eq!(cli.color, ColorChoice::Always);

    assert!(Cli::try_parse_from(["sekai", "--color=maybe", "log"]).is_err());
}

#[test]
fn reports_json_mode_per_command() {
    let cli = Cli::try_parse_from(["sekai", "log"]).expect("parses");
    assert!(!cli.command.output_json());

    let cli = Cli::try_parse_from(["sekai", "log", "--json"]).expect("parses");
    assert!(cli.command.output_json());

    let cli = Cli::try_parse_from(["sekai", "commit"]).expect("parses");
    assert!(!cli.command.output_json());

    let cli = Cli::try_parse_from(["sekai", "diff", "--in", "overworld:0,0"]).expect("parses");
    assert!(!cli.command.output_json());

    let cli =
        Cli::try_parse_from(["sekai", "diff", "--in", "overworld:0,0", "--json"]).expect("parses");
    assert!(cli.command.output_json());

    let cli = Cli::try_parse_from(["sekai", "checkout", H3]).expect("parses");
    assert!(!cli.command.output_json());

    let cli = Cli::try_parse_from(["sekai", "checkout", H3, "--json"]).expect("parses");
    assert!(cli.command.output_json());
    assert_eq!(cli.command.name(), "checkout");

    let cli = Cli::try_parse_from(["sekai", "cache", "clean"]).expect("parses");
    assert!(!cli.command.output_json());

    let cli = Cli::try_parse_from(["sekai", "cache", "clean", "--json"]).expect("parses");
    assert!(cli.command.output_json());
    assert_eq!(cli.command.name(), "cache");
}

#[test]
fn parses_cache_subcommands() {
    let cli = Cli::try_parse_from(["sekai", "--store", "s", "cache", "clean"])
        .expect("cache clean parses");
    assert!(matches!(
        cli.command,
        Command::Cache {
            action: CacheCommand::Clean { json: false }
        }
    ));
    assert_eq!(cli.command.name(), "cache");
    assert!(!cli.command.output_json());
    assert_eq!(cli.store, Some("s".into()));

    let cli =
        Cli::try_parse_from(["sekai", "cache", "clean", "--json"]).expect("cache clean --json");
    assert!(matches!(
        cli.command,
        Command::Cache {
            action: CacheCommand::Clean { json: true }
        }
    ));
    assert!(cli.command.output_json());

    for args in [
        // The subcommand is required; there is no bare `cache` run.
        vec!["sekai", "cache"],
        vec!["sekai", "cache", "bogus"],
        // Clean takes no world argument and has no phases to time.
        vec!["sekai", "cache", "clean", "world"],
        vec!["sekai", "cache", "clean", "--timing"],
    ] {
        assert!(
            Cli::try_parse_from(&args).is_err(),
            "{args:?} must be refused"
        );
    }
}

#[test]
fn parses_scope_subcommands() {
    let cli = Cli::try_parse_from([
        "sekai",
        "scope",
        "create",
        "hub/nether",
        "--in",
        "nether:1,-2",
        "--in",
        "nether:3,4..-5,6",
        "--region",
        "overworld:-1,0",
        "--force",
        "--json",
    ])
    .expect("scope create parses");
    let Command::Scope {
        action:
            ScopeCommand::Create {
                name,
                areas,
                region,
                force: true,
                json: true,
            },
    } = &cli.command
    else {
        panic!("unexpected command: {:?}", cli.command);
    };
    assert_eq!(name.as_str(), "hub/nether");
    assert_eq!(
        areas
            .iter()
            .map(|spec| (spec.dim, spec.rect))
            .collect::<Vec<_>>(),
        [
            (Dimension::NETHER, Rect::new(1, -2, 1, -2)),
            (Dimension::NETHER, Rect::new(-5, 4, 3, 6)),
        ]
    );
    assert_eq!(
        region.iter().map(RegionSpec::area).collect::<Vec<_>>(),
        [(Dimension::OVERWORLD, Rect::region(-1, 0))]
    );
    assert_eq!(cli.command.name(), "scope");
    assert!(cli.command.output_json());

    let cli = Cli::try_parse_from(["sekai", "scope", "create", "spawn", "--region", "ow:0,0"])
        .expect("scope create with only --region parses");
    assert!(matches!(
        &cli.command,
        Command::Scope {
            action: ScopeCommand::Create { areas, force: false, json: false, .. }
        } if areas.is_empty()
    ));
    assert!(!cli.command.output_json());

    // A scope name refuses no path shape (ADR-0035).
    Cli::try_parse_from(["sekai", "scope", "create", "a/../b", "--in", "ow:0,0"])
        .expect("scope create with a dotted path parses");

    let cli = Cli::try_parse_from(["sekai", "scope", "delete", "spawn"]).expect("scope delete");
    assert!(matches!(
        &cli.command,
        Command::Scope {
            action: ScopeCommand::Delete { name, json: false }
        } if name.as_str() == "spawn"
    ));

    let cli = Cli::try_parse_from(["sekai", "scope", "list", "--json"]).expect("scope list");
    assert!(matches!(
        cli.command,
        Command::Scope {
            action: ScopeCommand::List { json: true }
        }
    ));
    assert!(cli.command.output_json());

    for args in [
        // A scope selects nothing without an area.
        vec!["sekai", "scope", "create", "spawn"],
        // A whole dimension is not a chunk rectangle (ADR-0021).
        vec!["sekai", "scope", "create", "spawn", "--in", "nether"],
        // A scope covers every region family.
        vec![
            "sekai", "scope", "create", "spawn", "--in", "ow:0,0", "--kind", "region",
        ],
        vec!["sekai", "scope", "create", "bad name", "--in", "ow:0,0"],
        vec!["sekai", "scope", "delete", "spawn", "--force"],
        vec!["sekai", "scope", "list", "--force"],
    ] {
        assert!(
            Cli::try_parse_from(&args).is_err(),
            "{args:?} must be refused"
        );
    }

    // Errors advertise only the forms a scope area accepts.
    for area in ["bogus:0,0", "nether"] {
        let err = Cli::try_parse_from(["sekai", "scope", "create", "spawn", "--in", area])
            .expect_err("invalid scope area")
            .to_string();
        assert!(
            err.contains("expected DIM:X,Z or DIM:X0,Z0..X1,Z1"),
            "{err}"
        );
    }
}

#[test]
fn parses_scope_selection_flag() {
    for args in [
        vec!["sekai", "commit", "--scope", "spawn"],
        vec!["sekai", "status", "--scope", "spawn"],
        vec!["sekai", "checkout", H3, "--scope", "spawn"],
        vec!["sekai", "diff", "--scope", "spawn"],
    ] {
        let cli = Cli::try_parse_from(&args).expect("--scope parses");
        let selection = match &cli.command {
            Command::Commit { selection, .. }
            | Command::Status { selection, .. }
            | Command::Checkout(CheckoutArgs { selection, .. })
            | Command::Diff(DiffArgs { selection, .. }) => selection,
            other => panic!("unexpected command: {other:?}"),
        };
        assert!(
            matches!(selection, Selection::Scope(name) if name.as_str() == "spawn"),
            "{args:?}"
        );
    }

    // A selection is spelled inline or named, never a mix, and names one
    // scope at most.
    for args in [
        vec!["sekai", "commit", "--scope", "spawn", "--in", "ow:0,0"],
        vec!["sekai", "commit", "--scope", "spawn", "--region", "ow:0,0"],
        vec!["sekai", "commit", "--scope", "spawn", "--kind", "region"],
        vec!["sekai", "commit", "--scope", "spawn", "--scope", "hub"],
        vec!["sekai", "commit", "--scope", "bad name"],
    ] {
        assert!(
            Cli::try_parse_from(&args).is_err(),
            "{args:?} must be refused"
        );
    }
}

/// The branch action of a parsed invocation.
fn branch(args: &[&str]) -> BranchAction {
    match Cli::try_parse_from(args).expect("branch parses").command {
        Command::Branch(command) => command.action,
        other => panic!("expected branch, got {other:?}"),
    }
}

fn names(raw: &[&str]) -> Vec<sekai_app::RefName> {
    raw.iter()
        .map(|name| sekai_app::RefName::parse(name).unwrap())
        .collect()
}

#[test]
fn parses_branch_modes_as_git_does() {
    let patterns = |raw: &[&str]| raw.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert_eq!(
        branch(&["sekai", "branch"]),
        BranchAction::List { patterns: vec![] }
    );
    assert_eq!(
        branch(&["sekai", "branch", "--list", "feat*", "fix*"]),
        BranchAction::List {
            patterns: patterns(&["feat*", "fix*"])
        }
    );
    assert_eq!(
        branch(&["sekai", "branch", "-f", "dev", "main~1"]),
        BranchAction::Create {
            name: names(&["dev"])[0].clone(),
            start: Some(Revision::parse("main~1").unwrap()),
            force: true,
        }
    );
    assert_eq!(
        branch(&["sekai", "branch", "-d", "a", "b"]),
        BranchAction::Delete {
            names: names(&["a", "b"]),
            force: false
        }
    );
    assert_eq!(
        branch(&["sekai", "branch", "-D", "a"]),
        BranchAction::Delete {
            names: names(&["a"]),
            force: true
        }
    );
    assert_eq!(
        branch(&["sekai", "branch", "-m", "new"]),
        BranchAction::Rename {
            old: None,
            new: names(&["new"])[0].clone(),
            force: false
        }
    );
    assert_eq!(
        branch(&["sekai", "branch", "-M", "old", "new"]),
        BranchAction::Rename {
            old: Some(names(&["old"])[0].clone()),
            new: names(&["new"])[0].clone(),
            force: true
        }
    );
    assert_eq!(
        branch(&["sekai", "branch", "--show-current"]),
        BranchAction::ShowCurrent
    );

    // The mode options exclude each other, and `-f` only moves a branch.
    for args in [
        vec!["sekai", "branch", "-d", "-m", "a"],
        vec!["sekai", "branch", "-d", "-D", "a"],
        vec!["sekai", "branch", "--list", "--show-current"],
        vec!["sekai", "branch", "-f", "-d", "a"],
        vec!["sekai", "branch", "--show-current", "a"],
        vec!["sekai", "branch", "-m"],
        vec!["sekai", "branch", "-m", "a", "b", "c"],
    ] {
        assert!(Cli::try_parse_from(&args).is_err(), "{args:?} is refused");
    }
    let cli = Cli::try_parse_from(["sekai", "branch", "--json"]).expect("parses");
    assert_eq!(cli.command.name(), "branch");
    assert!(cli.command.output_json());
}

#[test]
fn parses_tag_modes_as_git_does() {
    let tag = |args: &[&str]| match Cli::try_parse_from(args).expect("tag parses").command {
        Command::Tag(command) => command.action,
        other => panic!("expected tag, got {other:?}"),
    };
    assert_eq!(tag(&["sekai", "tag"]), TagAction::List { patterns: vec![] });
    assert_eq!(
        tag(&["sekai", "tag", "-l", "v*"]),
        TagAction::List {
            patterns: vec!["v*".to_owned()]
        }
    );
    assert_eq!(
        tag(&["sekai", "tag", "-f", "v1", "main"]),
        TagAction::Create {
            name: names(&["v1"])[0].clone(),
            target: Some(Revision::parse("main").unwrap()),
            force: true,
        }
    );
    assert_eq!(
        tag(&["sekai", "tag", "-d", "v1", "v2"]),
        TagAction::Delete {
            names: names(&["v1", "v2"])
        }
    );
    for args in [
        vec!["sekai", "tag", "-l", "-d", "v1"],
        vec!["sekai", "tag", "-f", "-d", "v1"],
        vec!["sekai", "tag", "v1", "main", "extra"],
    ] {
        assert!(Cli::try_parse_from(&args).is_err(), "{args:?} is refused");
    }
}

#[test]
fn parses_reflog_and_worktree() {
    let reflog = |args: &[&str]| match Cli::try_parse_from(args).expect("reflog parses").command {
        Command::Reflog(command) => command.log,
        other => panic!("expected reflog, got {other:?}"),
    };
    assert_eq!(
        reflog(&["sekai", "reflog"]),
        sekai_app::ReflogSelector::Head
    );
    assert_eq!(
        reflog(&["sekai", "reflog", "show"]),
        sekai_app::ReflogSelector::Head
    );
    assert_eq!(
        reflog(&["sekai", "reflog", "HEAD"]),
        sekai_app::ReflogSelector::Head
    );
    assert_eq!(
        reflog(&["sekai", "reflog", "show", "dev"]),
        sekai_app::ReflogSelector::Ref(names(&["dev"])[0].clone())
    );

    let cli = Cli::try_parse_from(["sekai", "worktree", "add", "../w", "v1", "--json"])
        .expect("worktree add parses");
    assert!(matches!(
        &cli.command,
        Command::Worktree {
            action: WorktreeCommand::Add { commit_ish: Some(rev), json: true, .. }
        } if rev.as_str() == "v1"
    ));
    assert_eq!(cli.command.name(), "worktree");
    assert!(Cli::try_parse_from(["sekai", "worktree", "list"]).is_ok());
    assert!(Cli::try_parse_from(["sekai", "worktree", "remove", "w"]).is_ok());
    assert!(
        Cli::try_parse_from(["sekai", "worktree", "add", "w", "--progress", "--json"]).is_err()
    );
}
