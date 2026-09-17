use clap::Parser as _;
use sekai_app::Dimension;
use sekai_cli::{
    cli::{
        Cli, Command, DebugCommand, DiffArgs, ExportFlavor, OnMissingBlob, OnMissingFile,
        Selection, TimingArgs,
    },
    style::ColorChoice,
};

#[test]
fn parses_subcommands() {
    let cli = Cli::try_parse_from(["sekai", "backup", "world"]).expect("backup parses");
    assert!(matches!(cli.command, Command::Backup { .. }));
    assert_eq!(cli.store, "sekai-store");

    let cli = Cli::try_parse_from(["sekai", "--store", "s", "rollback", "w", "3"])
        .expect("rollback parses");
    assert!(matches!(&cli.command, Command::Rollback { snapshot, .. } if snapshot == "3"));

    assert!(Cli::try_parse_from(["sekai", "rollback", "w"]).is_err());
}

#[test]
fn parses_progress_flag() {
    let cli = Cli::try_parse_from(["sekai", "backup", "world", "--progress"])
        .expect("backup --progress parses");
    assert!(matches!(
        cli.command,
        Command::Backup { progress: true, .. }
    ));

    let cli = Cli::try_parse_from(["sekai", "backup", "world"]).expect("backup parses");
    assert!(matches!(
        cli.command,
        Command::Backup {
            progress: false,
            ..
        }
    ));

    // Progress is human-only: refuses `--json` loudly instead of
    // polluting the machine-readable contract.
    assert!(Cli::try_parse_from(["sekai", "backup", "world", "--progress", "--json"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "backup", "world", "--json", "--progress"]).is_err());

    for args in [
        vec!["sekai", "rollback", "w", "3", "--progress"],
        vec!["sekai", "gc", "--progress"],
        vec!["sekai", "diff", "--in", "overworld:0,0", "--progress"],
    ] {
        Cli::try_parse_from(args).expect("progress parses");
    }
    assert!(Cli::try_parse_from(["sekai", "rollback", "w", "3", "--progress", "--json"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "gc", "--progress", "--json"]).is_err());
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
fn parses_diff_command() {
    let cli = Cli::try_parse_from(["sekai", "diff", "1", "2", "--in", "overworld:10,-5"])
        .expect("diff parses");
    assert!(matches!(
        cli.command,
        Command::Diff(DiffArgs {
            world: None,
            old_snapshot: Some(_),
            new_snapshot: Some(_),
            output: TimingArgs {
                timing: false,
                json: false,
            },
            show_values: false,
            ..
        })
    ));
    if let Command::Diff(args) = &cli.command {
        // Default kinds cover all three families.
        assert_eq!(args.selection.explicit_chunks().len(), 3);
        assert_eq!(args.old_snapshot.as_deref(), Some("1"));
        assert_eq!(args.new_snapshot.as_deref(), Some("2"));
    } else {
        panic!("expected diff");
    }

    let cli = Cli::try_parse_from(["sekai", "diff", "@stable", "2"]).expect("diff tag refs parse");
    if let Command::Diff(args) = &cli.command {
        assert_eq!(args.old_snapshot.as_deref(), Some("@stable"));
    } else {
        panic!("expected diff");
    }

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
            old_snapshot: None,
            new_snapshot: None,
            output: TimingArgs {
                timing: false,
                json: false,
            },
            show_values: false,
            ..
        })
    ));
    if let Command::Diff(args) = &cli.command {
        let chunks = args.selection.explicit_chunks();
        assert_eq!(chunks.len(), 1);
        assert_eq!((chunks[0].x, chunks[0].z), (10, -5));
    } else {
        panic!("expected diff");
    }

    let cli = Cli::try_parse_from([
        "sekai",
        "diff",
        "1",
        "2",
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
        assert!(args.selection.has_broad_areas());
        assert_eq!(args.selection.explicit_chunks().len(), 1);
        let scope = args.selection.owned_scope();
        assert!(matches!(scope, sekai_app::Scope::Select { .. }));
    } else {
        panic!("expected diff");
    }

    // Removed flags fail loudly.
    assert!(Cli::try_parse_from(["sekai", "diff", "--cx", "0"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "diff", "--dimension"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "backup", "w", "--dim", "nether"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "diff", "--in", "bogus"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "diff", "--in", "overworld:bogus"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "diff", "--in", "overworld:1,2,3"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "diff", "--region", "0,0"]).is_err());
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
fn parses_selection_for_backup_and_rollback() {
    let cli = Cli::try_parse_from(["sekai", "backup", "--in", "nether", "w"])
        .expect("backup --in parses");
    assert!(matches!(cli.command, Command::Backup { .. }));
    if let Command::Backup { selection, .. } = &cli.command {
        assert!(selection.has_broad_areas());
        assert!(selection.explicit_chunks().is_empty());
    } else {
        panic!("expected backup");
    }

    let cli = Cli::try_parse_from(["sekai", "rollback", "w", "3", "--region", "nether:1,-1"])
        .expect("rollback --region parses");
    assert!(matches!(&cli.command, Command::Rollback { snapshot, .. } if snapshot == "3"));

    // `--in` and `--region` compose; kinds are repeatable.
    assert!(
        Cli::try_parse_from([
            "sekai",
            "backup",
            "w",
            "--region",
            "nether:0,0",
            "--in",
            "nether"
        ])
        .is_ok()
    );
    let cli = Cli::try_parse_from([
        "sekai", "backup", "w", "--kind", "region", "--kind", "entities",
    ])
    .expect("repeatable --kind parses");
    if let Command::Backup { selection, .. } = &cli.command {
        assert_eq!(selection.kinds().len(), 2);
    } else {
        panic!("expected backup");
    }
}

#[test]
fn selection_owned_scope_round_trips() {
    use sekai_app::Scope;

    let base = Selection {
        areas: Vec::new(),
        region: Vec::new(),
        kind: Vec::new(),
    };
    assert_eq!(base.owned_scope(), Scope::World);
    let dim = Selection {
        areas: vec!["nether".parse().unwrap()],
        ..base.clone()
    };
    let scope = dim.owned_scope();
    assert!(matches!(scope, Scope::Select { .. }));
    let chunks = Selection {
        areas: vec!["overworld:1,-2".parse().unwrap()],
        kind: vec![sekai_app::RegionKind::REGION],
        ..base
    };
    let scope = chunks.owned_scope();
    let coord =
        sekai_app::ChunkCoord::new(Dimension::OVERWORLD, sekai_app::RegionKind::REGION, 1, -2);
    assert!(scope.contains(coord));
    assert!(!scope.contains(sekai_app::ChunkCoord::new(
        Dimension::OVERWORLD,
        sekai_app::RegionKind::REGION,
        2,
        -2
    )));
}

#[test]
fn parses_timing_and_debug_scan() {
    let cli = Cli::try_parse_from(["sekai", "backup", "--timing", "world"])
        .expect("backup --timing parses");
    assert!(matches!(
        cli.command,
        Command::Backup {
            output: TimingArgs { timing: true, .. },
            ..
        }
    ));

    let cli = Cli::try_parse_from(["sekai", "backup", "--timing", "--json", "world"])
        .expect("backup --timing --json parses");
    assert!(matches!(
        cli.command,
        Command::Backup {
            output: TimingArgs {
                timing: true,
                json: true,
            },
            ..
        }
    ));

    let cli = Cli::try_parse_from(["sekai", "rollback", "--timing", "w", "3"])
        .expect("rollback --timing parses");
    assert!(matches!(
        cli.command,
        Command::Rollback {
            output: TimingArgs { timing: true, .. },
            ..
        }
    ));

    let cli = Cli::try_parse_from(["sekai", "rollback", "--json", "w", "3"])
        .expect("rollback --json parses");
    assert!(matches!(
        cli.command,
        Command::Rollback {
            output: TimingArgs { json: true, .. },
            ..
        }
    ));

    let cli =
        Cli::try_parse_from(["sekai", "rollback", "w", "3"]).expect("rollback defaults parse");
    assert!(matches!(
        cli.command,
        Command::Rollback {
            keep_post_snapshot_files: false,
            keep_post_snapshot_chunks: false,
            keep_tombstoned_chunks: false,
            on_missing_blob: OnMissingBlob::Abort,
            on_missing_file: OnMissingFile::SiblingFirst,
            ..
        }
    ));

    let cli = Cli::try_parse_from([
        "sekai",
        "rollback",
        "w",
        "3",
        "--keep-post-snapshot-files",
        "--keep-post-snapshot-chunks",
        "--keep-tombstoned-chunks",
        "--on-missing-blob",
        "skip-chunk",
        "--on-missing-file",
        "error",
    ])
    .expect("rollback strategy flags parse");
    assert!(matches!(
        cli.command,
        Command::Rollback {
            keep_post_snapshot_files: true,
            keep_post_snapshot_chunks: true,
            keep_tombstoned_chunks: true,
            on_missing_blob: OnMissingBlob::SkipChunk,
            on_missing_file: OnMissingFile::Error,
            ..
        }
    ));
    assert!(
        Cli::try_parse_from(["sekai", "rollback", "w", "3", "--on-missing-blob", "ignore"])
            .is_err(),
        "unknown missing-blob policy is refused"
    );

    let cli = Cli::try_parse_from(["sekai", "export", "3", "out"]).expect("export parses");
    assert!(matches!(
        &cli.command,
        Command::Export {
            snapshot,
            flavor: ExportFlavor::Legacy,
            on_missing_blob: OnMissingBlob::Abort,
            output: TimingArgs {
                timing: false,
                json: false,
            },
            ..
        } if snapshot == "3"
    ));

    let cli = Cli::try_parse_from([
        "sekai",
        "export",
        "3",
        "out",
        "--flavor",
        "bukkit",
        "--base",
        "myworld",
        "--on-missing-blob",
        "skip-chunk",
        "--timing",
        "--json",
    ])
    .expect("export options parse");
    assert!(matches!(
        &cli.command,
        Command::Export {
            snapshot,
            flavor: ExportFlavor::Bukkit,
            on_missing_blob: OnMissingBlob::SkipChunk,
            output: TimingArgs {
                timing: true,
                json: true,
            },
            ..
        } if snapshot == "3"
    ));
    assert!(
        Cli::try_parse_from(["sekai", "export", "3", "out", "--flavor", "tarball"]).is_err(),
        "unknown export flavor is refused"
    );

    let cli = Cli::try_parse_from(["sekai", "tag", "stable", "3"]).expect("tag create parses");
    assert!(matches!(
        &cli.command,
        Command::Tag {
            name,
            snapshot: Some(_),
            delete: false,
            force: false,
            json: false,
        } if name.as_ref().is_some_and(|n| n.as_str() == "stable")
    ));

    let cli = Cli::try_parse_from(["sekai", "tag", "stable", "@prev", "--force", "--json"])
        .expect("tag force parses");
    assert!(matches!(
        &cli.command,
        Command::Tag {
            force: true,
            json: true,
            ..
        }
    ));
    assert_eq!(cli.command.name(), "tag");
    assert!(cli.command.output_json());

    let cli =
        Cli::try_parse_from(["sekai", "tag", "stable", "--delete"]).expect("tag delete parses");
    assert!(matches!(&cli.command, Command::Tag { delete: true, .. }));

    let cli = Cli::try_parse_from(["sekai", "tag"]).expect("bare tag lists");
    assert!(matches!(
        &cli.command,
        Command::Tag {
            name: None,
            snapshot: None,
            delete: false,
            ..
        }
    ));

    assert!(
        Cli::try_parse_from(["sekai", "tag", "stable", "3", "--delete"]).is_err(),
        "tag delete with snapshot is refused"
    );
    assert!(
        Cli::try_parse_from(["sekai", "tag", "bad name", "3"]).is_err(),
        "invalid tag name is refused"
    );

    let cli = Cli::try_parse_from(["sekai", "list", "--json"]).expect("list --json parses");
    assert!(matches!(cli.command, Command::List { json: true }));

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
            },
            ..
        })
    ));

    let cli = Cli::try_parse_from(["sekai", "debug", "scan", "world"]).expect("debug scan parses");
    assert!(matches!(cli.command, Command::Debug { .. }));

    let cli = Cli::try_parse_from(["sekai", "debug", "scan", "world", "--timing", "--json"])
        .expect("scan --timing --json parses");
    assert!(matches!(
        cli.command,
        Command::Debug {
            debug: DebugCommand::Scan {
                output: TimingArgs {
                    timing: true,
                    json: true,
                },
                ..
            }
        }
    ));
}

#[test]
fn parses_gc_command() {
    let cli = Cli::try_parse_from(["sekai", "gc"]).expect("gc parses");
    assert!(matches!(
        cli.command,
        Command::Gc {
            dry_run: false,
            output: TimingArgs {
                timing: false,
                json: false,
            },
            progress: false,
        }
    ));

    let cli = Cli::try_parse_from(["sekai", "gc", "--dry-run", "--json"])
        .expect("gc --dry-run --json parses");
    assert!(matches!(
        cli.command,
        Command::Gc {
            dry_run: true,
            output: TimingArgs { json: true, .. },
            ..
        }
    ));

    let cli = Cli::try_parse_from(["sekai", "gc", "--timing", "--json"])
        .expect("gc --timing --json parses");
    assert!(matches!(
        cli.command,
        Command::Gc {
            output: TimingArgs {
                timing: true,
                json: true,
            },
            ..
        }
    ));

    Cli::try_parse_from(["sekai", "gc", "--dry-run", "--timing", "--json"])
        .expect("gc --dry-run --timing --json parses");
    assert!(
        Cli::try_parse_from(["sekai", "gc", "--dry-run", "--progress"]).is_err(),
        "gc --dry-run --progress is refused"
    );
}

#[test]
fn parses_color_flag() {
    let cli = Cli::try_parse_from(["sekai", "backup", "world"]).expect("backup parses");
    assert_eq!(cli.color, ColorChoice::Auto);

    let cli = Cli::try_parse_from(["sekai", "--color=never", "list"]).expect("parses");
    assert_eq!(cli.color, ColorChoice::Never);

    let cli = Cli::try_parse_from(["sekai", "--color", "always", "list"]).expect("parses");
    assert_eq!(cli.color, ColorChoice::Always);

    assert!(Cli::try_parse_from(["sekai", "--color=maybe", "list"]).is_err());
}

#[test]
fn reports_json_mode_per_command() {
    let cli = Cli::try_parse_from(["sekai", "list"]).expect("parses");
    assert!(!cli.command.output_json());

    let cli = Cli::try_parse_from(["sekai", "list", "--json"]).expect("parses");
    assert!(cli.command.output_json());

    let cli = Cli::try_parse_from(["sekai", "backup", "w"]).expect("parses");
    assert!(!cli.command.output_json());

    let cli = Cli::try_parse_from(["sekai", "diff", "--in", "overworld:0,0"]).expect("parses");
    assert!(!cli.command.output_json());

    let cli =
        Cli::try_parse_from(["sekai", "diff", "--in", "overworld:0,0", "--json"]).expect("parses");
    assert!(cli.command.output_json());

    let cli = Cli::try_parse_from(["sekai", "export", "3", "out"]).expect("parses");
    assert!(!cli.command.output_json());

    let cli = Cli::try_parse_from(["sekai", "export", "3", "out", "--json"]).expect("parses");
    assert!(cli.command.output_json());
    assert_eq!(cli.command.name(), "export");
}
