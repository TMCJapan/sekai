//! The commands driven through the built binary from inside a world, as a
//! user runs them, checking the wording Git prints (ADR-0036).

use std::path::{Path, PathBuf};
use std::process::Command;

/// A world with an empty region file, which `init` recognizes, in a temp
/// dir removed with the guard.
struct World {
    root: PathBuf,
    _guard: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        let guard = tempfile::Builder::new()
            .prefix("sekai-cli-git-shape-")
            .tempdir()
            .unwrap();
        let root = guard.path().canonicalize().unwrap().join("world");
        std::fs::create_dir_all(root.join("region")).unwrap();
        std::fs::write(root.join("region/r.0.0.mca"), []).unwrap();
        let world = Self {
            root,
            _guard: guard,
        };
        world.ok(&["init"]);
        world
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.parent().unwrap().join(relative)
    }

    /// Run `sekai <args>` in the world, returning stdout and stderr.
    fn run(&self, args: &[&str]) -> (bool, String, String) {
        run_in(&self.root, args)
    }

    /// Run a command that must succeed, returning stdout and stderr.
    fn ok(&self, args: &[&str]) -> (String, String) {
        let (ok, out, err) = self.run(args);
        assert!(ok, "{args:?} failed: {err}");
        (out, err)
    }

    /// Run a command that must fail, returning stderr.
    fn fails(&self, args: &[&str]) -> String {
        let (ok, _, err) = self.run(args);
        assert!(!ok, "{args:?} succeeded");
        err
    }

    /// Record a commit, returning its ID.
    fn commit(&self) -> String {
        let (out, _) = self.ok(&["commit", "--json"]);
        let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
        doc["result"]["commit"].as_str().unwrap().to_owned()
    }
}

fn run_in(dir: impl AsRef<Path>, args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_sekai"))
        .arg("--color=never")
        .args(args)
        .current_dir(dir.as_ref())
        .output()
        .unwrap();
    (
        output.status.success(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn an_unborn_branch_reads_as_git_reads_one() {
    let world = World::new();
    let (out, _) = world.ok(&["status"]);
    assert!(
        out.starts_with("On branch main\n\nNo commits yet\n"),
        "{out}"
    );
    assert!(
        world
            .fails(&["log"])
            .contains("your current branch 'main' does not have any commits yet")
    );
    assert!(world.fails(&["diff"]).contains("unknown revision: HEAD"));
    assert!(
        world
            .fails(&["checkout"])
            .contains("You are on a branch yet to be born")
    );

    let (out, _) = world.ok(&["commit"]);
    let root = world.commit();
    assert!(out.starts_with("[main (root-commit) "), "{out}");
    let (out, _) = world.ok(&["status"]);
    assert!(out.starts_with("On branch main\n"), "{out}");
    let (out, _) = world.ok(&["log", "--oneline", "--decorate"]);
    assert!(
        out.starts_with(&format!("{root} (HEAD -> main)\n")),
        "{out}"
    );
}

#[test]
fn switching_prints_git_messages_and_detaches_with_advice() {
    let world = World::new();
    let first = world.commit();
    world.ok(&["branch", "dev"]);
    let (out, _) = world.ok(&["branch"]);
    assert_eq!(out, "  dev\n* main\n");

    let (_, err) = world.ok(&["checkout", "dev"]);
    assert_eq!(err, "Switched to branch 'dev'\n");
    let (_, err) = world.ok(&["checkout", "dev"]);
    assert_eq!(err, "Already on 'dev'\n");
    let (out, err) = world.ok(&["checkout", "HEAD"]);
    assert_eq!((out.as_str(), err.as_str()), ("", ""));

    world.ok(&["tag", "v1"]);
    let (_, err) = world.ok(&["checkout", "v1"]);
    assert!(err.starts_with("Note: switching to 'v1'.\n"), "{err}");
    assert!(err.contains("  sekai branch <new-branch-name>\n  sekai checkout <new-branch-name>\n"));
    assert!(!err.contains("git switch"), "{err}");
    assert!(err.ends_with(&format!("HEAD is now at {first}\n")), "{err}");
    let (out, _) = world.ok(&["status"]);
    assert!(
        out.starts_with(&format!("HEAD detached at {first}\n")),
        "{out}"
    );

    // A commit made while detached moves HEAD away from where it was put.
    let (out, _) = world.ok(&["commit"]);
    assert!(out.starts_with("[detached HEAD "), "{out}");
    let second = out
        .lines()
        .next()
        .unwrap()
        .trim_start_matches("[detached HEAD ")
        .trim_end_matches(']')
        .to_owned();
    let (out, _) = world.ok(&["status"]);
    assert!(
        out.starts_with(&format!("HEAD detached at {second}\n")),
        "{out}"
    );
    let (out, _) = world.ok(&["branch"]);
    assert_eq!(
        out,
        format!("* (HEAD detached at {second})\n  dev\n  main\n")
    );

    // Switching between detached commits gives no advice again.
    let (_, err) = world.ok(&["checkout", &first]);
    assert_eq!(err, format!("HEAD is now at {first}\n"));

    let (out, _) = world.ok(&["reflog"]);
    let newest = out.lines().next().unwrap();
    assert!(
        newest.starts_with(&format!("{first} HEAD@{{0}}: checkout: moving from ")),
        "{out}"
    );
    assert!(newest.ends_with(&format!(" to {first}")), "{out}");
    assert!(
        out.ends_with(&format!("{first} HEAD@{{5}}: commit (initial): \n")),
        "{out}"
    );
}

#[test]
fn branches_and_tags_take_git_modes() {
    let world = World::new();
    let first = world.commit();
    world.ok(&["branch", "side"]);
    world.ok(&["checkout", "side"]);
    let second = world.commit();
    world.ok(&["checkout", "main"]);

    assert!(
        world
            .fails(&["branch", "-d", "side"])
            .contains("the branch 'side' is not fully merged")
    );
    let (out, _) = world.ok(&["branch", "-D", "side"]);
    assert_eq!(out, format!("Deleted branch side (was {second}).\n"));

    world.ok(&["branch", "-m", "trunk"]);
    let (out, _) = world.ok(&["branch", "--show-current"]);
    assert_eq!(out, "trunk\n");
    let (out, _) = world.ok(&["branch", "--show-current", "--json"]);
    assert!(
        out.contains(r#""result":{"name":"refs/heads/trunk"}"#),
        "{out}"
    );
    assert!(
        world
            .fails(&["branch", "--", "-x"])
            .contains("invalid ref name '-x': ref name starts with '-'")
    );

    world.ok(&["tag", "v1"]);
    world.ok(&["tag", "v2"]);
    let (out, _) = world.ok(&["tag", "-l", "v1*"]);
    assert_eq!(out, "v1\n");
    let (out, _) = world.ok(&["tag", "-f", "v1", &first]);
    assert!(out.is_empty(), "the tag did not move: {out}");
    let (out, _) = world.ok(&["tag", "-f", "v2", &second]);
    assert_eq!(out, format!("Updated tag 'v2' (was {first})\n"));
    let (out, _) = world.ok(&["tag", "-d", "v1", "v2"]);
    assert_eq!(
        out,
        format!("Deleted tag 'v1' (was {first})\nDeleted tag 'v2' (was {second})\n")
    );
    assert!(
        world
            .fails(&["tag", "--", "-x"])
            .contains("invalid ref name '-x': ref name starts with '-'")
    );
}

#[test]
fn worktrees_refuse_their_branches_naming_the_worktree_root() {
    let world = World::new();
    let commit = world.commit();
    let (_, err) = world.ok(&["worktree", "add", "../wt"]);
    assert_eq!(
        err,
        format!("Preparing worktree (new branch 'wt')\nHEAD is now at {commit}\n")
    );
    let linked = world.path("wt");
    let linked_str = linked.display().to_string();

    let (out, _) = world.ok(&["worktree", "list"]);
    let main = world.root.display().to_string();
    let width = main.len().max(linked_str.len());
    assert_eq!(
        out,
        format!("{main:<width$} {commit} [main]\n{linked_str:<width$} {commit} [wt]\n")
    );
    let (out, _) = world.ok(&["branch"]);
    assert_eq!(out, "* main\n+ wt\n");

    let at = format!("used by worktree at '{linked_str}'");
    assert!(
        world
            .fails(&["checkout", "wt"])
            .contains(&format!("'wt' is already {at}"))
    );
    assert!(
        world
            .fails(&["branch", "-d", "wt"])
            .contains(&format!("cannot delete branch 'wt' {at}"))
    );
    assert!(
        world
            .fails(&["branch", "-f", "wt"])
            .contains(&format!("cannot force update the branch 'wt' {at}"))
    );
    // The linked worktree names the main one by its root too.
    let (ok, _, err) = run_in(&linked, &["checkout", "main"]);
    assert!(!ok);
    assert!(
        err.contains(&format!("'main' is already used by worktree at '{main}'")),
        "{err}"
    );

    world.ok(&["worktree", "remove", "wt"]);
    let (out, _) = world.ok(&["worktree", "list", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(doc["result"].as_array().unwrap().len(), 1);
    assert_eq!(doc["result"][0]["head"]["ref"], "refs/heads/main");
}

#[test]
fn log_walks_from_head_newest_first_with_decorations() {
    let world = World::new();
    let first = world.commit();
    world.ok(&["tag", "v1"]);
    world.ok(&["branch", "dev"]);
    let second = world.commit();

    let (out, _) = world.ok(&["log", "--decorate"]);
    let mut entries = out.split("\n\n");
    let newest = entries.next().unwrap();
    assert!(
        newest.starts_with(&format!("commit {second} (HEAD -> main)\nDate:   ")),
        "{out}"
    );
    let oldest = entries.next().unwrap();
    assert!(
        oldest.starts_with(&format!("commit {first} (tag: v1, dev)\n")),
        "{out}"
    );

    let (out, _) = world.ok(&["log", "--oneline", "--no-decorate", "--reverse"]);
    assert_eq!(out, format!("{first}\n{second}\n"));
    let (out, _) = world.ok(&["log", "--oneline", "--no-decorate", "dev..main"]);
    assert_eq!(out, format!("{second}\n"));
    let (out, _) = world.ok(&["diff", "dev..main"]);
    assert_eq!(out, "No differences found.\n");

    let (out, _) = world.ok(&["log", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        doc["result"][0]["refs"],
        serde_json::json!(["refs/heads/main"])
    );
    assert_eq!(doc["result"][0]["head"], true);
    assert_eq!(
        doc["result"][1]["refs"],
        serde_json::json!(["refs/tags/v1", "refs/heads/dev"])
    );
}

#[test]
fn a_selection_restores_an_area_without_moving_head() {
    let world = World::new();
    let first = world.commit();
    world.ok(&["branch", "dev"]);
    world.ok(&["branch", "feature"]);
    let (out, _) = world.ok(&["branch", "--list", "d*", "f*"]);
    assert_eq!(out, "  dev\n  feature\n");

    world.commit();
    let (out, err) = world.ok(&["checkout", "dev", "--in", "overworld"]);
    assert!(err.is_empty(), "a restore prints no switch message: {err}");
    assert!(
        out.starts_with(&format!("commit {first} restored: ")),
        "{out}"
    );
    let (out, _) = world.ok(&["branch", "--show-current"]);
    assert_eq!(out, "main\n");
    let (out, _) = world.ok(&["checkout", "-f"]);
    assert!(out.starts_with("commit "), "{out}");
}

#[test]
fn a_name_that_is_both_a_branch_and_a_tag_is_an_error() {
    let world = World::new();
    let _ = world.commit();
    world.ok(&["branch", "dev"]);
    world.ok(&["tag", "dev"]);

    // A checkout argument names the branch silently (the branch-first
    // rule), but a revision resolves it ambiguously.
    world.ok(&["checkout", "dev"]);
    let err = world.fails(&["log", "dev"]);
    assert!(
        err.contains("refname 'dev' is ambiguous"),
        "the error names the ambiguity: {err}"
    );
}

#[test]
fn json_mode_reports_head_in_one_envelope() {
    let world = World::new();
    let commit = world.commit();
    world.ok(&["branch", "dev"]);
    world.ok(&["checkout", "dev"]);
    let (out, err) = world.ok(&["checkout", "dev", "--json"]);
    assert!(err.is_empty(), "stderr stays silent in JSON mode: {err}");
    let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(doc.get("warnings").is_none(), "{out}");
    assert_eq!(doc["result"]["head"]["ref"], "refs/heads/dev");

    // Doing nothing still prints one envelope.
    let (out, _) = world.ok(&["checkout", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(doc["result"]["files_written"], 0);
    assert_eq!(doc["result"]["head"]["commit"], commit.as_str());
    assert!(doc.get("warnings").is_none(), "{out}");

    let (out, _) = world.ok(&["status", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(doc["result"]["head"]["ref"], "refs/heads/dev");
    let (out, _) = world.ok(&["commit", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(doc["result"]["head"]["commit"], doc["result"]["commit"]);
}
