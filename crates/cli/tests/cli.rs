use clap::Parser as _;

const H1: &str = "0101010101010101010101010101010101010101010101010101010101010101";
const H2: &str = "0202020202020202020202020202020202020202020202020202020202020202";
const H3: &str = "0303030303030303030303030303030303030303030303030303030303030303";
use sekai_app::{Dimension, Rect, Revision};
use sekai_cli::{
    cli::{
        BranchArgs, CacheCommand, Cli, Command, ConfigArgs, DiffArgs, InlineSelection,
        OnMissingBlob, OnMissingFile, RegionSpec, ScopeCommand, Selection, TimingArgs,
        WorktreeCommand,
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

#[test]
fn parses_subcommands() {
    let cli = Cli::try_parse_from(["sekai", "commit", "world"]).expect("commit parses");
    assert!(matches!(cli.command, Command::Commit { .. }));
    assert_eq!(cli.store, ".sekai");

    let cli = Cli::try_parse_from(["sekai", "--store", "s", "checkout", "w", H3])
        .expect("checkout parses");
    assert!(matches!(&cli.command, Command::Checkout { commit, .. } if commit.as_str() == H3));

    let cli =
        Cli::try_parse_from(["sekai", "checkout", "w", "main"]).expect("checkout bare ref parses");
    assert!(matches!(&cli.command, Command::Checkout { commit, .. } if commit.as_str() == "main"));
    let cli = Cli::try_parse_from(["sekai", "checkout", "w", "feature/nightly"])
        .expect("checkout bare ref parses");
    assert!(matches!(
        &cli.command,
        Command::Checkout { commit, .. } if commit.as_str() == "feature/nightly"
    ));

    assert!(Cli::try_parse_from(["sekai", "checkout", "w"]).is_err());

    let cli = Cli::try_parse_from(["sekai", "checkout", "w", H3]).expect("checkout parses");
    assert!(matches!(cli.command, Command::Checkout { jobs: 0, .. }));
    let cli = Cli::try_parse_from(["sekai", "checkout", "w", H3, "--jobs", "4"])
        .expect("checkout --jobs parses");
    assert!(matches!(cli.command, Command::Checkout { jobs: 4, .. }));

    let cli = Cli::try_parse_from(["sekai", "status", "w"]).expect("status parses");
    assert!(matches!(
        cli.command,
        Command::Status {
            jobs: 0,
            output: TimingArgs {
                timing: false,
                json: false,
                progress: false,
            },
            ..
        }
    ));
    assert_eq!(cli.command.name(), "status");
    assert!(!cli.command.output_json());

    let cli = Cli::try_parse_from(["sekai", "status", "w", "--timing", "--json", "--jobs", "4"])
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
fn parses_restore() {
    // The source is optional (HEAD at runtime); the selection flags parse
    // like every other scoped command.
    let cli = Cli::try_parse_from(["sekai", "restore", "w", "--in", "overworld:0,0"])
        .expect("restore parses");
    assert!(matches!(
        &cli.command,
        Command::Restore(args) if args.source.is_none() && !args.overlay && args.jobs == 0
    ));
    assert_eq!(cli.command.name(), "restore");
    assert!(!cli.command.output_json());

    // git restore's `-s <tree>` short form, plus the long spelling.
    for source in ["-s", "--source"] {
        let cli = Cli::try_parse_from([
            "sekai",
            "restore",
            "w",
            source,
            "main~1",
            "--region",
            "overworld:0,0",
        ])
        .expect("restore --source parses");
        assert!(matches!(
            &cli.command,
            Command::Restore(args) if ref_str(args.source.as_ref()) == Some("main~1")
        ));
    }

    let cli = Cli::try_parse_from([
        "sekai",
        "restore",
        "w",
        "--overlay",
        "--in",
        "overworld:0,0",
        "--json",
        "--jobs",
        "4",
    ])
    .expect("restore options parse");
    assert!(matches!(
        &cli.command,
        Command::Restore(args) if args.overlay && args.jobs == 4
    ));
    assert!(cli.command.output_json());

    // Malformed revisions are refused at parse time, like checkout's.
    for bad in ["", "HEAD^", "main~x", "@{1}"] {
        assert!(
            Cli::try_parse_from(["sekai", "restore", "w", "-s", bad, "--in", "overworld:0,0"])
                .is_err(),
            "{bad:?} must be refused"
        );
    }
    assert!(
        Cli::try_parse_from([
            "sekai",
            "restore",
            "w",
            "--in",
            "overworld:0,0",
            "--progress",
            "--json"
        ])
        .is_err()
    );
}

#[test]
fn parses_progress_flag() {
    let cli = Cli::try_parse_from(["sekai", "commit", "world", "--progress"])
        .expect("commit --progress parses");
    assert!(matches!(
        cli.command,
        Command::Commit {
            output: TimingArgs { progress: true, .. },
            ..
        }
    ));

    let cli = Cli::try_parse_from(["sekai", "commit", "world"]).expect("commit parses");
    assert!(matches!(
        cli.command,
        Command::Commit {
            output: TimingArgs {
                progress: false,
                ..
            },
            ..
        }
    ));

    // Progress is human-only: refuses `--json` loudly instead of
    // polluting the machine-readable contract.
    assert!(Cli::try_parse_from(["sekai", "commit", "world", "--progress", "--json"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "commit", "world", "--json", "--progress"]).is_err());

    for args in [
        vec!["sekai", "checkout", "w", H3, "--progress"],
        vec!["sekai", "diff", "--in", "overworld:0,0", "--progress"],
    ] {
        Cli::try_parse_from(args).expect("progress parses");
    }
    assert!(Cli::try_parse_from(["sekai", "checkout", "w", H3, "--progress", "--json"]).is_err());
    assert!(
        Cli::try_parse_from([
            "sekai",
            "diff",
            "--in",
            "overworld:0,0",
            "--progress",
            "--json"
        ])
        .is_err()
    );
}

#[test]
fn parses_no_cache_flag() {
    // Default: the pre-commit cache update pass runs.
    let cli = Cli::try_parse_from(["sekai", "commit", "world"]).expect("commit parses");
    assert!(matches!(
        cli.command,
        Command::Commit {
            no_cache: false,
            ..
        }
    ));

    let cli = Cli::try_parse_from(["sekai", "commit", "world", "--no-cache"])
        .expect("commit --no-cache parses");
    assert!(matches!(
        cli.command,
        Command::Commit { no_cache: true, .. }
    ));

    // The flag is commit-only: other commands refuse it.
    for args in [
        vec!["sekai", "status", "w", "--no-cache"],
        vec!["sekai", "checkout", "w", H3, "--no-cache"],
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
            world: None,
            old_commit: Some(_),
            new_commit: Some(_),
            output: TimingArgs {
                timing: false,
                json: false,
                progress: false,
            },
            show_values: false,
            ..
        })
    ));
    if let Command::Diff(args) = &cli.command {
        // Default kinds cover all three families.
        assert_eq!(inline(&args.selection).explicit_chunks().len(), 3);
        assert_eq!(ref_str(args.old_commit.as_ref()), Some(H1));
        assert_eq!(ref_str(args.new_commit.as_ref()), Some(H2));
    } else {
        panic!("expected diff");
    }

    let err = Cli::try_parse_from(["sekai", "diff", "@stable", H2])
        .expect_err("@-prefixed tag refs are no longer accepted");
    assert!(
        err.to_string()
            .contains("expected one ~<n> after the starting point"),
        "the @ prefix is refused as a malformed revision: {err}"
    );

    let cli = Cli::try_parse_from([
        "sekai",
        "diff",
        "--world",
        "world",
        "--in",
        "overworld:10,-5",
        "--kind",
        "region",
    ])
    .expect("diff with world parses");
    assert!(matches!(
        cli.command,
        Command::Diff(DiffArgs {
            world: Some(_),
            old_commit: None,
            new_commit: None,
            output: TimingArgs {
                timing: false,
                json: false,
                progress: false,
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

    // The `tag` subcommand is gone.
    assert!(Cli::try_parse_from(["sekai", "tag", "list"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "tag"]).is_err());

    // Removed flags fail loudly.
    assert!(Cli::try_parse_from(["sekai", "diff", "--cx", "0"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "diff", "--dimension"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "commit", "w", "--dim", "nether"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "diff", "--in", "bogus"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "diff", "--in", "overworld:bogus"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "diff", "--in", "overworld:1,2,3"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "diff", "--region", "0,0"]).is_err());
}

#[test]
fn validates_revisions_at_parse_time() {
    // Malformed revisions are refused before any code runs, so they can
    // never touch the store.
    for args in [
        vec!["sekai", "checkout", "w", ""],
        vec!["sekai", "checkout", "w", "@123"],
        vec!["sekai", "checkout", "w", "@bad name"],
        vec!["sekai", "checkout", "w", "bad name"],
        vec!["sekai", "checkout", "w", "HEAD^"],
        vec!["sekai", "checkout", "w", "main~x"],
        vec!["sekai", "checkout", "w", "~1"],
        vec!["sekai", "checkout", "w", "/main"],
        vec!["sekai", "checkout", "w", "main/"],
        vec!["sekai", "checkout", "w", "a//b"],
        vec!["sekai", "checkout", "w", "a/../b"],
        vec!["sekai", "checkout", "w", "end."],
        vec!["sekai", "diff", H1, "bad name"],
        // The `ref` command is gone; branches come back as `branch`.
        vec!["sekai", "ref", "list"],
        vec!["sekai", "ref"],
    ] {
        assert!(Cli::try_parse_from(&args).is_err(), "{args:?} is refused");
    }

    for args in [
        vec!["sekai", "checkout", "w", "main"],
        vec!["sekai", "checkout", "w", "feature/nightly"],
        vec!["sekai", "checkout", "w", "v1.2_x-1"],
        vec!["sekai", "diff", H1, H2],
        // A digit-only name is a ref: commit IDs are 64 hex characters.
        vec!["sekai", "checkout", "w", "123"],
        vec!["sekai", "checkout", "w", "HEAD"],
        vec!["sekai", "checkout", "w", "@"],
        vec!["sekai", "checkout", "w", "HEAD~1"],
        vec!["sekai", "checkout", "w", "@~"],
        vec!["sekai", "checkout", "w", "main~12"],
        vec!["sekai", "diff", "HEAD~1", "HEAD"],
    ] {
        Cli::try_parse_from(&args).unwrap_or_else(|err| panic!("{args:?} parses: {err}"));
    }

    // `--world` compares against at most one commit; a second positional
    // used to be ignored silently and is now refused.
    Cli::try_parse_from(["sekai", "diff", "--world", "w", "main"])
        .expect("diff --world with one commit parses");
    assert!(
        Cli::try_parse_from(["sekai", "diff", "--world", "w", H1, H2]).is_err(),
        "diff --world with two commits is refused"
    );

    // The refusal names what a revision may start from and why the name
    // failed, so the wording is part of the contract the parse error
    // carries.
    let err = Cli::try_parse_from(["sekai", "checkout", "w", "a..b"])
        .expect_err("a name with `..` is refused");
    let message = err.to_string();
    assert!(
        message.contains("not a commit ID or ref name"),
        "the refusal names the accepted shapes: {message}"
    );
    assert!(
        message.contains("contains '..'"),
        "the refusal names the broken rule: {message}"
    );
    for step in ["main~x", "main~1~2", "HEAD~~"] {
        let err = Cli::try_parse_from(["sekai", "checkout", "w", step])
            .expect_err("anything but one ~<n> after the start is refused");
        assert!(
            err.to_string()
                .contains("expected one ~<n> after the starting point"),
            "the refusal names the step syntax for {step}: {err}"
        );
    }
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
    let cli = Cli::try_parse_from(["sekai", "commit", "--in", "nether", "w"])
        .expect("commit --in parses");
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

    let cli = Cli::try_parse_from(["sekai", "checkout", "w", H3, "--region", "nether:1,-1"])
        .expect("checkout --region parses");
    assert!(matches!(&cli.command, Command::Checkout { commit, .. } if commit.as_str() == H3));

    // `--in` and `--region` compose; kinds are repeatable.
    assert!(
        Cli::try_parse_from([
            "sekai",
            "commit",
            "w",
            "--region",
            "nether:0,0",
            "--in",
            "nether"
        ])
        .is_ok()
    );
    let cli = Cli::try_parse_from([
        "sekai", "commit", "w", "--kind", "region", "--kind", "entities",
    ])
    .expect("repeatable --kind parses");
    if let Command::Commit { selection, .. } = &cli.command {
        assert_eq!(inline(selection).kinds().len(), 2);
    } else {
        panic!("expected commit");
    }

    // Codes outside the known families are refused at parse time instead of
    // widening into an unknown kind.
    assert!(Cli::try_parse_from(["sekai", "commit", "w", "--kind", "9"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "commit", "w", "--kind", "blocks"]).is_err());
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
    let cli = Cli::try_parse_from(["sekai", "commit", "--timing", "world"])
        .expect("commit --timing parses");
    assert!(matches!(
        cli.command,
        Command::Commit {
            output: TimingArgs { timing: true, .. },
            ..
        }
    ));

    let cli = Cli::try_parse_from(["sekai", "commit", "--timing", "--json", "world"])
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

    let cli = Cli::try_parse_from(["sekai", "checkout", "--timing", "w", H3])
        .expect("checkout --timing parses");
    assert!(matches!(
        cli.command,
        Command::Checkout {
            output: TimingArgs { timing: true, .. },
            ..
        }
    ));

    let cli = Cli::try_parse_from(["sekai", "checkout", "--json", "w", H3])
        .expect("checkout --json parses");
    assert!(matches!(
        cli.command,
        Command::Checkout {
            output: TimingArgs { json: true, .. },
            ..
        }
    ));

    let cli = Cli::try_parse_from(["sekai", "checkout", "w", H3]).expect("checkout defaults parse");
    assert!(matches!(
        cli.command,
        Command::Checkout {
            keep_post_commit_files: false,
            keep_post_commit_chunks: false,
            keep_tombstoned_chunks: false,
            on_missing_blob: OnMissingBlob::Abort,
            on_missing_file: OnMissingFile::SiblingFirst,
            ..
        }
    ));

    let cli = Cli::try_parse_from([
        "sekai",
        "checkout",
        "w",
        H3,
        "--keep-post-commit-files",
        "--keep-post-commit-chunks",
        "--keep-tombstoned-chunks",
        "--on-missing-blob",
        "skip-chunk",
        "--on-missing-file",
        "error",
    ])
    .expect("checkout strategy flags parse");
    assert!(matches!(
        cli.command,
        Command::Checkout {
            keep_post_commit_files: true,
            keep_post_commit_chunks: true,
            keep_tombstoned_chunks: true,
            on_missing_blob: OnMissingBlob::SkipChunk,
            on_missing_file: OnMissingFile::Error,
            ..
        }
    ));
    assert!(
        Cli::try_parse_from(["sekai", "checkout", "w", H3, "--on-missing-blob", "ignore"]).is_err(),
        "unknown missing-blob policy is refused"
    );

    let cli = Cli::try_parse_from(["sekai", "log", "--json"]).expect("log --json parses");
    assert!(matches!(cli.command, Command::Log { json: true, .. }));

    let cli = Cli::try_parse_from(["sekai", "log", "--stat"]).expect("log --stat parses");
    assert!(matches!(
        cli.command,
        Command::Log {
            json: false,
            stat: true
        }
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
    assert_eq!(cli.store, ".sekai");

    let cli = Cli::try_parse_from(["sekai", "--store", "s", "init", "--json"])
        .expect("init --json parses");
    assert!(matches!(cli.command, Command::Init { json: true }));
    assert!(cli.command.output_json());
    assert_eq!(cli.store, "s");

    // Init takes no world argument and has no phases to time.
    assert!(Cli::try_parse_from(["sekai", "init", "world"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "init", "--timing"]).is_err());
}

#[test]
fn parses_color_flag() {
    let cli = Cli::try_parse_from(["sekai", "commit", "world"]).expect("commit parses");
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

    let cli = Cli::try_parse_from(["sekai", "commit", "w"]).expect("parses");
    assert!(!cli.command.output_json());

    let cli = Cli::try_parse_from(["sekai", "diff", "--in", "overworld:0,0"]).expect("parses");
    assert!(!cli.command.output_json());

    let cli =
        Cli::try_parse_from(["sekai", "diff", "--in", "overworld:0,0", "--json"]).expect("parses");
    assert!(cli.command.output_json());

    let cli = Cli::try_parse_from(["sekai", "checkout", "w", H3]).expect("parses");
    assert!(!cli.command.output_json());

    let cli = Cli::try_parse_from(["sekai", "checkout", "w", H3, "--json"]).expect("parses");
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
    assert_eq!(cli.store, "s");

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

    // A scope name refuses no path shape: it never reads as a revision.
    Cli::try_parse_from(["sekai", "scope", "create", "a/../b", "--in", "ow:0,0"])
        .expect("a path-shaped scope name parses");

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
        vec!["sekai", "commit", "w", "--scope", "spawn"],
        vec!["sekai", "status", "w", "--scope", "spawn"],
        vec!["sekai", "checkout", "w", H3, "--scope", "spawn"],
        vec!["sekai", "diff", "--scope", "spawn"],
    ] {
        let cli = Cli::try_parse_from(&args).expect("--scope parses");
        let selection = match &cli.command {
            Command::Commit { selection, .. }
            | Command::Status { selection, .. }
            | Command::Checkout { selection, .. }
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
        vec!["sekai", "commit", "w", "--scope", "spawn", "--in", "ow:0,0"],
        vec![
            "sekai", "commit", "w", "--scope", "spawn", "--region", "ow:0,0",
        ],
        vec![
            "sekai", "commit", "w", "--scope", "spawn", "--kind", "region",
        ],
        vec!["sekai", "commit", "w", "--scope", "spawn", "--scope", "hub"],
        vec!["sekai", "commit", "w", "--scope", "bad name"],
    ] {
        assert!(
            Cli::try_parse_from(&args).is_err(),
            "{args:?} must be refused"
        );
    }
}

/// The `branch` arguments, or a panic for another command.
fn branch_args(argv: &[&str]) -> BranchArgs {
    match Cli::try_parse_from(argv).expect("branch parses").command {
        Command::Branch(args) => args,
        other => panic!("expected branch, got {other:?}"),
    }
}

#[test]
fn parses_branch_listing() {
    assert!(!branch_args(&["sekai", "branch"]).json);

    let cli = Cli::try_parse_from(["sekai", "branch", "--json"]).expect("parses");
    assert_eq!(cli.command.name(), "branch");
    assert!(cli.command.output_json());
}

#[test]
fn branch_takes_no_patterns() {
    for argv in [
        &["sekai", "branch", "dev"][..],
        &["sekai", "branch", "--list", "m*"],
    ] {
        let err = Cli::try_parse_from(argv).expect_err("no patterns");
        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
    }
}

/// The `worktree list` arguments, or a panic for another command.
fn worktree_list_args(argv: &[&str]) -> sekai_cli::cli::WorktreeListArgs {
    match Cli::try_parse_from(argv)
        .expect("worktree list parses")
        .command
    {
        Command::Worktree {
            action: WorktreeCommand::List(args),
        } => args,
        other => panic!("expected worktree list, got {other:?}"),
    }
}

#[test]
fn parses_worktree_listing() {
    let args = worktree_list_args(&["sekai", "worktree", "list"]);
    assert!(!args.porcelain && !args.z && !args.verbose && !args.json);

    let cli = Cli::try_parse_from(["sekai", "worktree", "list"]).expect("parses");
    assert_eq!(cli.command.name(), "worktree");
    assert!(!cli.command.output_json());

    let cli = Cli::try_parse_from(["sekai", "worktree", "list", "--json"]).expect("parses");
    assert!(cli.command.output_json());

    // Git's flags: --porcelain, -z, and -v.
    let args = worktree_list_args(&["sekai", "worktree", "list", "--porcelain", "-z"]);
    assert!(args.porcelain && args.z);
    let args = worktree_list_args(&["sekai", "worktree", "list", "-v"]);
    assert!(args.verbose);
    let args = worktree_list_args(&["sekai", "worktree", "list", "--verbose"]);
    assert!(args.verbose);
}

#[test]
fn worktree_list_mirrors_git_flag_rules() {
    for argv in [
        // The subcommand is required; there is no bare `worktree` run.
        &["sekai", "worktree"][..],
        &["sekai", "worktree", "bogus"],
        // List takes no positional argument.
        &["sekai", "worktree", "list", "extra"],
        // -z requires --porcelain, as in Git.
        &["sekai", "worktree", "list", "-z"],
        // --verbose and --porcelain are exclusive, as in Git.
        &["sekai", "worktree", "list", "-v", "--porcelain"],
        // JSON is its own machine format; porcelain is another.
        &["sekai", "worktree", "list", "--json", "--porcelain"],
        &["sekai", "worktree", "list", "--json", "-z"],
        // Git's --expire annotates prunable worktrees; sekai has no prune.
        &["sekai", "worktree", "list", "--expire", "now"],
    ] {
        assert!(
            Cli::try_parse_from(argv).is_err(),
            "{argv:?} must be refused"
        );
    }
}

/// The `config` arguments, or a panic for another command.
fn config_args(argv: &[&str]) -> ConfigArgs {
    match Cli::try_parse_from(argv).expect("config parses").command {
        Command::Config(args) => args,
        other => panic!("expected config, got {other:?}"),
    }
}

#[test]
fn parses_config_actions() {
    use sekai_storage_local::ConfigKey;

    // Set: key plus value.
    let args = config_args(&["sekai", "config", "user.name", "foo"]);
    assert_eq!(args.key, Some(ConfigKey::UserName));
    assert_eq!(args.value.as_deref(), Some("foo"));
    assert!(!args.get && !args.unset && !args.list && !args.json);

    // Get: bare key or `--get`; keys are case-insensitive.
    for argv in [
        &["sekai", "config", "user.name"][..],
        &["sekai", "config", "--get", "user.name"],
        &["sekai", "config", "USER.NAME"],
    ] {
        let args = config_args(argv);
        assert_eq!(args.key, Some(ConfigKey::UserName));
        assert!(args.value.is_none());
    }

    // Unset, list (long and short), name-only, default, and JSON.
    assert!(config_args(&["sekai", "config", "user.name", "--unset"]).unset);
    for argv in [
        &["sekai", "config", "--list"][..],
        &["sekai", "config", "-l"],
    ] {
        let args = config_args(argv);
        assert!(args.list && args.key.is_none());
    }
    assert!(config_args(&["sekai", "config", "-l", "--name-only"]).name_only);
    assert_eq!(
        config_args(&["sekai", "config", "user.name", "--default", "fallback"])
            .default
            .as_deref(),
        Some("fallback")
    );
    let cli = Cli::try_parse_from(["sekai", "config", "--list", "--json"]).expect("parses");
    assert_eq!(cli.command.name(), "config");
    assert!(cli.command.output_json());
}

#[test]
fn config_refuses_invalid_combinations() {
    for argv in [
        // A key is required for every action but `--list`.
        &["sekai", "config"][..],
        &["sekai", "config", "--get"],
        &["sekai", "config", "--unset"],
        // Actions are mutually exclusive, and `--list` takes no key.
        &["sekai", "config", "user.name", "foo", "--get"],
        &["sekai", "config", "user.name", "foo", "--unset"],
        &["sekai", "config", "user.name", "foo", "--default", "x"],
        &["sekai", "config", "user.name", "--get", "--unset"],
        &["sekai", "config", "--list", "user.name"],
        &["sekai", "config", "--list", "--default", "x"],
        &["sekai", "config", "user.name", "--unset", "--default", "x"],
        // `--name-only` modifies `--list` and stays human-only.
        &["sekai", "config", "user.name", "--name-only"],
        &["sekai", "config", "--list", "--name-only", "--json"],
        // Unknown keys and empty values are refused at parse time.
        &["sekai", "config", "user.email", "foo"],
        &["sekai", "config", "user.name", ""],
        // No location flags: the store is the only scope.
        &["sekai", "config", "--global", "user.name", "foo"],
        &["sekai", "config", "--file", "other.toml", "user.name"],
        // Git's multivar actions have no meaning for typed single values.
        &["sekai", "config", "--add", "user.name", "foo"],
        &["sekai", "config", "--get-all", "user.name"],
        &["sekai", "config", "--unset-all", "user.name"],
        // At most two positionals: key and value.
        &["sekai", "config", "user.name", "foo", "bar"],
    ] {
        assert!(
            Cli::try_parse_from(argv).is_err(),
            "{argv:?} must be refused"
        );
    }
}

/// The `worktree remove` arguments, or a panic for another command.
fn worktree_remove_args(argv: &[&str]) -> sekai_cli::cli::WorktreeRemoveArgs {
    match Cli::try_parse_from(argv)
        .expect("worktree remove parses")
        .command
    {
        Command::Worktree {
            action: WorktreeCommand::Remove(args),
        } => args,
        other => panic!("expected worktree remove, got {other:?}"),
    }
}

#[test]
fn parses_worktree_removal() {
    let args = worktree_remove_args(&["sekai", "worktree", "remove", "../wt"]);
    assert_eq!(args.worktree, std::path::PathBuf::from("../wt"));
    assert!(!args.force && !args.json);

    // Git's force: -f / --force, repeatable (-f -f).
    let args = worktree_remove_args(&["sekai", "worktree", "remove", "-f", "../wt"]);
    assert!(args.force);
    let args = worktree_remove_args(&["sekai", "worktree", "remove", "--force", "../wt"]);
    assert!(args.force);
    let args = worktree_remove_args(&["sekai", "worktree", "remove", "-f", "-f", "../wt"]);
    assert!(args.force);

    let args = worktree_remove_args(&["sekai", "worktree", "remove", "--json", "../wt"]);
    assert!(args.json);
    let cli =
        Cli::try_parse_from(["sekai", "worktree", "remove", "--json", "../wt"]).expect("parses");
    assert_eq!(cli.command.name(), "worktree");
    assert!(cli.command.output_json());
}

#[test]
fn worktree_remove_mirrors_git_flag_rules() {
    for argv in [
        // The worktree positional is required.
        &["sekai", "worktree", "remove"][..],
        // Only one worktree per run.
        &["sekai", "worktree", "remove", "a", "b"],
        // Git's remove has no listing formats.
        &["sekai", "worktree", "remove", "--porcelain", "a"],
        &["sekai", "worktree", "remove", "-z", "a"],
    ] {
        assert!(
            Cli::try_parse_from(argv).is_err(),
            "{argv:?} must be refused"
        );
    }
}
