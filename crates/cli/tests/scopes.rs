//! Named scopes driven through the CLI over a local store.

use clap::Parser as _;
use sekai_app::{Dimension, Rect, SekaiInstance};
use sekai_cli::cli::Cli;

/// A temp dir whose name starts with the calling test's name (tempfile
/// appends a random suffix), removed when its guard is dropped.
fn tempdir(name: &str) -> (std::path::PathBuf, tempfile::TempDir) {
    let guard = tempfile::Builder::new()
        .prefix(&format!("sekai-cli-{name}-"))
        .tempdir()
        .unwrap();
    (guard.path().to_path_buf(), guard)
}

async fn run(args: &[&str]) -> anyhow::Result<()> {
    let cli = Cli::try_parse_from(args).expect("valid invocation");
    let printer = sekai_cli::envelope::Printer::new(
        sekai_cli::style::Styler::new(cli.color),
        cli.command.output_json(),
    );
    sekai_cli::commands::run(&cli, &printer).await
}

#[tokio::test]
async fn saved_scopes_select_by_name() {
    let (root, _guard) = tempdir("scope-flow");
    let world = root.join("world");
    // An empty region file is the empty region, and enough for `init` to
    // recognize the world.
    std::fs::create_dir_all(world.join("region")).unwrap();
    std::fs::write(world.join("region/r.0.0.mca"), []).unwrap();
    let store = root.join("store");
    let store = store.to_str().expect("temporary store path is UTF-8");
    let world = world.to_str().expect("temporary world path is UTF-8");

    run(&["sekai", "--world", world, "--store", store, "init"])
        .await
        .expect("init creates the store");
    run(&["sekai", "--world", world, "--store", store, "commit"])
        .await
        .expect("commit records the empty world");
    run(&[
        "sekai",
        "--world",
        world,
        "--store",
        store,
        "scope",
        "create",
        "spawn",
        "--in",
        "ow:1,2",
        "--region",
        "nether:-1,0",
    ])
    .await
    .expect("scope create");

    let listed = SekaiInstance::open_location(&outside(store))
        .await
        .unwrap()
        .list_scopes()
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name.as_str(), "spawn");
    let mut areas = listed[0].areas().to_vec();
    areas.sort_unstable();
    let mut expected = vec![
        (Dimension::OVERWORLD, Rect::new(1, 2, 1, 2)),
        (Dimension::NETHER, Rect::region(-1, 0)),
    ];
    expected.sort_unstable();
    assert_eq!(areas, expected);

    let err = run(&[
        "sekai", "--world", world, "--store", store, "scope", "create", "spawn", "--in", "ow:0,0",
    ])
    .await
    .expect_err("an existing name needs --force");
    assert!(
        err.chain()
            .any(|cause| cause.to_string() == "a scope named 'spawn' already exists")
    );
    run(&[
        "sekai", "--world", world, "--store", store, "scope", "create", "spawn", "--in", "ow:0,0",
        "--force",
    ])
    .await
    .expect("--force replaces the scope");

    for args in [
        vec![
            "sekai", "--world", world, "--store", store, "status", "--scope", "spawn",
        ],
        vec![
            "sekai", "--world", world, "--store", store, "commit", "--scope", "spawn",
        ],
        vec![
            "sekai", "--world", world, "--store", store, "diff", "--scope", "spawn",
        ],
        vec!["sekai", "--world", world, "--store", store, "scope", "list"],
    ] {
        run(&args)
            .await
            .unwrap_or_else(|err| panic!("{args:?} failed: {err:?}"));
    }

    let err = run(&[
        "sekai", "--world", world, "--store", store, "status", "--scope", "nope",
    ])
    .await
    .expect_err("an unknown scope fails");
    assert!(
        err.chain()
            .any(|cause| cause.to_string() == "no scope named 'nope'")
    );

    run(&[
        "sekai", "--world", world, "--store", store, "scope", "delete", "spawn",
    ])
    .await
    .expect("scope delete");
    let err = run(&[
        "sekai", "--world", world, "--store", store, "scope", "delete", "spawn",
    ])
    .await
    .expect_err("a deleted scope is gone");
    assert!(err.to_string().contains("no scope named 'spawn'"));
}

/// The store at `store`, opened outside every worktree.
fn outside(store: &str) -> sekai_app::Location {
    sekai_app::Location {
        store: store.into(),
        world: None,
    }
}
