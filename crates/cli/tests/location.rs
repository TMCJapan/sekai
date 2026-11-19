//! `--world` and `--store` locate the store and the worktree through
//! discovery, as Git finds `.git` (ADR-0036 *Locating the store and the
//! worktree*).

use std::path::{Path, PathBuf};

use clap::Parser as _;
use sekai_cli::cli::Cli;

/// A temp dir whose name starts with `name`, removed when its guard drops.
fn tempdir(name: &str) -> (PathBuf, tempfile::TempDir) {
    let guard = tempfile::Builder::new()
        .prefix(&format!("sekai-cli-{name}-"))
        .tempdir()
        .unwrap();
    (guard.path().canonicalize().unwrap(), guard)
}

/// A world `init` recognizes: an empty region file is the empty region.
fn world_at(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    std::fs::create_dir_all(path.join("region")).unwrap();
    std::fs::write(path.join("region/r.0.0.mca"), []).unwrap();
    path.to_str()
        .expect("temporary world path is UTF-8")
        .to_owned()
}

async fn run(args: &[&str]) -> anyhow::Result<()> {
    let cli = Cli::try_parse_from(args).expect("valid invocation");
    let printer = sekai_cli::envelope::Printer::new(
        sekai_cli::style::Styler::new(cli.color),
        cli.command.output_json(),
    );
    sekai_cli::commands::run(&cli, &printer).await
}

fn mentions(err: &anyhow::Error, text: &str) -> bool {
    err.chain().any(|cause| cause.to_string().contains(text))
}

#[tokio::test]
async fn a_world_with_its_own_store_needs_no_store_option() {
    let (root, _guard) = tempdir("main-worktree");
    let world = &world_at(root.join("world"));

    run(&["sekai", "--world", world, "init"])
        .await
        .expect("init creates the store at the world");
    assert!(sekai_app::local_store_exists(root.join("world/.sekai")));

    // Discovery walks up from a directory inside the world.
    let inside = root.join("world/region");
    let inside = inside.to_str().unwrap();
    for args in [
        vec!["sekai", "--world", inside, "commit"],
        vec!["sekai", "--world", inside, "status"],
        vec!["sekai", "--world", inside, "diff"],
        vec!["sekai", "--world", inside, "log"],
        vec!["sekai", "--world", inside, "cache", "update"],
    ] {
        run(&args)
            .await
            .unwrap_or_else(|err| panic!("{args:?} failed: {err:?}"));
    }
}

#[tokio::test]
async fn a_store_elsewhere_links_the_world() {
    let (root, _guard) = tempdir("linked-worktree");
    let world = &world_at(root.join("world"));
    let store = root.join("store");
    let store = store.to_str().unwrap();

    run(&["sekai", "--world", world, "--store", store, "init"])
        .await
        .expect("init registers the world in the store");
    assert!(
        root.join("world/.sekai").is_file(),
        "a link file names the store"
    );

    // The link file names the store, so later commands need only the world.
    run(&["sekai", "--world", world, "commit"])
        .await
        .expect("commit through the link file");
    run(&["sekai", "--world", world, "--store", store, "log"])
        .await
        .expect("log with the store given as well");
}

#[tokio::test]
async fn world_commands_refuse_to_run_outside_every_worktree() {
    let (root, _guard) = tempdir("outside");
    let world = &world_at(root.join("world"));
    let store = root.join("store");
    let store = store.to_str().unwrap();
    let elsewhere = root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let elsewhere = elsewhere.to_str().unwrap();

    run(&["sekai", "--world", world, "--store", store, "init"])
        .await
        .expect("init");

    for args in [
        vec!["sekai", "--world", elsewhere, "--store", store, "commit"],
        vec!["sekai", "--world", elsewhere, "--store", store, "status"],
        vec!["sekai", "--world", elsewhere, "--store", store, "diff"],
        vec![
            "sekai", "--world", elsewhere, "--store", store, "cache", "update",
        ],
    ] {
        let err = run(&args).await.expect_err("no worktree to work in");
        assert!(
            mentions(&err, "this operation must be run in a work tree"),
            "{args:?} failed for the wrong reason: {err:?}"
        );
    }
    // Store-wide commands still run with `--store` (ADR-0036).
    for args in [
        vec![
            "sekai", "--world", elsewhere, "--store", store, "scope", "list",
        ],
        vec![
            "sekai", "--world", elsewhere, "--store", store, "cache", "clean",
        ],
    ] {
        run(&args)
            .await
            .unwrap_or_else(|err| panic!("{args:?} failed: {err:?}"));
    }
}

#[tokio::test]
async fn without_a_sekai_or_a_store_discovery_fails() {
    let (root, _guard) = tempdir("no-sekai");
    let start = root.to_str().unwrap();
    let err = run(&["sekai", "--world", start, "log"])
        .await
        .expect_err("nothing to discover");
    assert!(mentions(&err, "not in a sekai world"), "{err:?}");
}

#[tokio::test]
async fn diff_on_an_unborn_head_fails_as_an_unknown_revision() {
    let (root, _guard) = tempdir("unborn-diff");
    let world = &world_at(root.join("world"));
    run(&["sekai", "--world", world, "init"])
        .await
        .expect("init");

    let err = run(&["sekai", "--world", world, "diff"])
        .await
        .expect_err("HEAD has no commit to compare with");
    assert!(mentions(&err, "unknown revision: HEAD"), "{err:?}");

    run(&["sekai", "--world", world, "commit"])
        .await
        .expect("commit");
    run(&["sekai", "--world", world, "diff"])
        .await
        .expect("diff against HEAD's commit");
}
