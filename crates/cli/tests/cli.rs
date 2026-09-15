use clap::Parser as _;
use sekai_app::Dimension;
use sekai_cli::{
    cli::{Cli, Command, DebugCommand, DiffArgs, Selection, TimingArgs, Xz},
    style::ColorChoice,
};

#[test]
fn parses_subcommands() {
    let cli = Cli::try_parse_from(["sekai", "backup", "world"]).expect("backup parses");
    assert!(matches!(cli.command, Command::Backup { .. }));
    assert_eq!(cli.store, "sekai-store");

    let cli = Cli::try_parse_from(["sekai", "--store", "s", "rollback", "w", "3"])
        .expect("rollback parses");
    assert!(matches!(cli.command, Command::Rollback { snapshot: 3, .. }));

    assert!(Cli::try_parse_from(["sekai", "rollback", "w"]).is_err());
}

#[test]
fn parses_diff_command() {
    let cli =
        Cli::try_parse_from(["sekai", "diff", "1", "2", "--chunk", "10,-5"]).expect("diff parses");
    assert!(matches!(
        cli.command,
        Command::Diff(DiffArgs {
            world: None,
            old_snapshot: Some(1),
            new_snapshot: Some(2),
            selection: Selection {
                region: None,
                dimension: false,
                ..
            },
            output: TimingArgs {
                timing: false,
                json: false,
            },
            show_values: false,
        })
    ));
    if let Command::Diff(args) = &cli.command {
        assert_eq!(args.selection.chunks().len(), 1);
        assert_eq!(args.selection.chunks()[0].x, 10);
    } else {
        panic!("expected diff");
    }

    let cli = Cli::try_parse_from(["sekai", "diff", "--world", "world", "--chunk", "10,-5"])
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

    let cli = Cli::try_parse_from(["sekai", "diff", "1", "2", "--chunk", "0,0", "--show-values"])
        .expect("diff --show-values parses");
    assert!(matches!(
        cli.command,
        Command::Diff(DiffArgs {
            show_values: true,
            ..
        })
    ));

    let cli = Cli::try_parse_from(["sekai", "diff", "--chunk", "0,0", "--chunk", "1,-1"])
        .expect("repeated --chunk parses");
    if let Command::Diff(args) = &cli.command {
        assert_eq!(args.selection.chunks().len(), 2);
    } else {
        panic!("expected diff");
    }
    let cli = Cli::try_parse_from(["sekai", "diff", "--region", "0,0"]).expect("region parses");
    assert!(matches!(
        cli.command,
        Command::Diff(DiffArgs {
            selection: Selection {
                region: Some(_),
                dimension: false,
                ..
            },
            ..
        })
    ));
    assert!(Cli::try_parse_from(["sekai", "diff", "--region", "0,0", "--dimension"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "diff", "--chunk", "0,0", "--region", "0,0"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "diff", "--chunk", "0,0", "--dimension"]).is_err());
    assert!(Cli::try_parse_from(["sekai", "diff", "--chunk", "bogus"]).is_err());
}

#[test]
fn parses_selection_for_backup_and_rollback() {
    let cli = Cli::try_parse_from(["sekai", "backup", "--dimension", "--dim", "nether", "w"])
        .expect("backup --dimension parses");
    assert!(matches!(
        cli.command,
        Command::Backup {
            selection: Selection {
                dimension: true,
                dim: Dimension::NETHER,
                ..
            },
            ..
        }
    ));

    let cli = Cli::try_parse_from(["sekai", "rollback", "w", "3", "--region", "1,-1"])
        .expect("rollback --region parses");
    assert!(matches!(
        cli.command,
        Command::Rollback {
            snapshot: 3,
            selection: Selection {
                region: Some(_),
                ..
            },
            ..
        }
    ));

    assert!(
        Cli::try_parse_from(["sekai", "backup", "w", "--region", "0,0", "--dimension"]).is_err()
    );
}

#[test]
fn parses_xz_pairs() {
    assert!(matches!(
        "10,-5".parse::<Xz>().expect("parses"),
        Xz { x: 10, z: -5 }
    ));
    assert!("bogus".parse::<Xz>().is_err());
    assert!("1".parse::<Xz>().is_err());
    assert!("1,2,3".parse::<Xz>().is_err());
    assert!("a,b".parse::<Xz>().is_err());
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

    let cli = Cli::try_parse_from(["sekai", "list", "--json"]).expect("list --json parses");
    assert!(matches!(cli.command, Command::List { json: true }));

    let cli = Cli::try_parse_from(["sekai", "diff", "--chunk", "0,0", "--timing", "--json"])
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

    let cli = Cli::try_parse_from(["sekai", "diff", "--chunk", "0,0"]).expect("parses");
    assert!(!cli.command.output_json());

    let cli = Cli::try_parse_from(["sekai", "diff", "--chunk", "0,0", "--json"]).expect("parses");
    assert!(cli.command.output_json());
}
