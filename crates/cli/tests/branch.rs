//! `sekai branch` run as a process over a local store, so stdout and the
//! exit code are what a script sees.

use std::path::Path;
use std::process::{Command, Output};

/// A temp dir whose name starts with the calling test's name (tempfile
/// appends a random suffix), removed when its guard is dropped.
fn tempdir(name: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("sekai-cli-{name}-"))
        .tempdir()
        .unwrap()
}

fn sekai(store: impl AsRef<Path>, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sekai"))
        .arg("--store")
        .arg(store.as_ref())
        .args(["--color", "never"])
        .args(args)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> &str {
    assert!(
        output.status.success(),
        "sekai failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::str::from_utf8(&output.stdout).unwrap()
}

fn init(root: impl AsRef<Path>) -> std::path::PathBuf {
    let store = root.as_ref().join("store");
    stdout(&sekai(&store, &["init"]));
    store
}

#[test]
fn lists_the_unborn_branch_of_a_fresh_store() {
    let root = tempdir("branch-fresh");
    let store = init(root.path());

    assert_eq!(stdout(&sekai(&store, &["branch"])), "* main\n");
    assert_eq!(
        stdout(&sekai(&store, &["branch", "--json"])),
        "{\"command\":\"branch\",\"status\":\"ok\",\"result\":[{\"name\":\"refs/heads/main\",\"commit\":null,\"head\":true}]}\n"
    );
}

#[test]
fn lists_the_branch_a_commit_moved() {
    let root = tempdir("branch-born");
    let store = init(root.path());
    let world = root.path().join("world");
    std::fs::create_dir_all(&world).unwrap();
    stdout(&sekai(&store, &["commit", world.to_str().unwrap()]));
    let log: serde_json::Value =
        serde_json::from_str(stdout(&sekai(&store, &["log", "--json"]))).unwrap();
    let commit = &log["result"][0]["id"];

    let listed: serde_json::Value =
        serde_json::from_str(stdout(&sekai(&store, &["branch", "--json"]))).unwrap();
    assert_eq!(
        listed["result"],
        serde_json::json!([{"name": "refs/heads/main", "commit": commit, "head": true}])
    );
}

#[test]
fn a_missing_store_fails_with_an_error_envelope() {
    let root = tempdir("branch-missing");
    let output = sekai(root.path().join("absent"), &["branch", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["command"], "branch");
    assert_eq!(envelope["status"], "error");
}
