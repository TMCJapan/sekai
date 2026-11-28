//! `sekai worktree list` and `sekai worktree remove` run as a process over
//! a local store, so stdout and the exit code are what a script sees.
//! Linked worktrees are registered through `LocalStore::add_worktree`: the
//! CLI has no `worktree add` yet.

use std::path::Path;
use std::process::{Command, Output};

use sekai_app::SekaiStoreOpener as _;
use sekai_storage_local::LocalStore;
use sekai_util::{RefName, worktree::Head};

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

/// The location a store link at `link` is listed at: its parent directory
/// resolved through the filesystem (macOS temp dirs sit behind a symlink),
/// without the verbatim prefix Windows resolves to.
fn link_key(link: impl AsRef<Path>) -> String {
    let link = link.as_ref();
    let parent = std::fs::canonicalize(link.parent().unwrap()).unwrap();
    let parent = parent.to_str().unwrap();
    let parent = parent.strip_prefix(r"\\?\").unwrap_or(parent);
    Path::new(parent)
        .join(link.file_name().unwrap())
        .to_str()
        .unwrap()
        .to_owned()
}

fn init(root: impl AsRef<Path>) -> std::path::PathBuf {
    let store = root.as_ref().join("store");
    stdout(&sekai(&store, &["init"]));
    store
}

/// Record one commit and return its ID.
fn first_commit(store: impl AsRef<Path>, root: impl AsRef<Path>) -> String {
    let store = store.as_ref();
    let world = root.as_ref().join("world");
    std::fs::create_dir_all(&world).unwrap();
    stdout(&sekai(store, &["commit", world.to_str().unwrap()]));
    let log: serde_json::Value =
        serde_json::from_str(stdout(&sekai(store, &["log", "--json"]))).unwrap();
    log["result"][0]["id"].as_str().unwrap().to_owned()
}

#[test]
fn lists_the_unborn_main_worktree_of_a_fresh_store() {
    let root = tempdir("worktree-fresh");
    let store = init(root.path());
    let path = std::path::absolute(&store).unwrap();
    let path = path.to_str().unwrap();

    assert_eq!(
        stdout(&sekai(&store, &["worktree", "list"])),
        format!("{path}  0000000 [main]\n")
    );
    assert_eq!(
        stdout(&sekai(&store, &["worktree", "list", "--porcelain"])),
        format!(
            "worktree {path}\nHEAD {zeros}\nbranch refs/heads/main\n\n",
            zeros = "0".repeat(64)
        )
    );
    let listed: serde_json::Value =
        serde_json::from_str(stdout(&sekai(&store, &["worktree", "list", "--json"]))).unwrap();
    assert_eq!(
        listed,
        serde_json::json!({
            "command": "worktree",
            "status": "ok",
            "result": [{
                "path": path,
                "commit": null,
                "branch": "refs/heads/main",
                "main": true,
            }],
        })
    );
}

#[test]
fn lists_the_commit_the_head_names() {
    let root = tempdir("worktree-born");
    let store = init(root.path());
    let commit = first_commit(&store, root.path());
    let path = std::path::absolute(&store).unwrap();
    let path = path.to_str().unwrap();

    assert_eq!(
        stdout(&sekai(&store, &["worktree", "list"])),
        format!("{path}  {abbrev} [main]\n", abbrev = &commit[..7])
    );
    assert_eq!(
        stdout(&sekai(&store, &["worktree", "list", "--porcelain"])),
        format!("worktree {path}\nHEAD {commit}\nbranch refs/heads/main\n\n")
    );
}

/// Register two linked worktrees whose paths sort around the store root:
/// `aaa/.sekai` before it, `zzz/.sekai` after it. The listing keeps the
/// main worktree first, as Git does, and resolves each HEAD: an unborn
/// branch shows zeros, a detached HEAD shows its commit.
#[tokio::test]
async fn lists_linked_worktrees_main_first() {
    let root = tempdir("worktree-linked");
    let store = init(root.path());
    let commit = first_commit(&store, root.path());

    let aaa = root.path().join("aaa");
    let zzz = root.path().join("zzz");
    std::fs::create_dir_all(&aaa).unwrap();
    std::fs::create_dir_all(&zzz).unwrap();
    let opened = LocalStore::open(&store).await.unwrap();
    opened
        .add_worktree(
            aaa.join(".sekai"),
            Head::Branch(RefName::parse("feature").unwrap()),
        )
        .await
        .unwrap();
    opened
        .add_worktree(zzz.join(".sekai"), Head::Detached(commit.parse().unwrap()))
        .await
        .unwrap();
    // The binary needs the store's session lock free.
    drop(opened);

    let main_path = std::path::absolute(&store).unwrap();
    let main_path = main_path.to_str().unwrap();
    let aaa_path = link_key(aaa.join(".sekai"));
    let zzz_path = link_key(zzz.join(".sekai"));
    let (aaa_path, zzz_path) = (aaa_path.as_str(), zzz_path.as_str());
    // Git pads each path toward the widest byte length plus two spaces
    // at the widest, and each HEAD toward the widest abbreviation (7
    // here: the store holds one commit) plus one space.
    let maxlen = [main_path, aaa_path, zzz_path]
        .iter()
        .map(|path| path.len())
        .max()
        .unwrap();
    let line = |path: &str, hex: &str, suffix: &str| {
        format!(
            "{path}{} {hex:<7} {suffix}\n",
            " ".repeat(maxlen + 1 - path.len())
        )
    };
    let abbrev = &commit[..7];
    assert_eq!(
        stdout(&sekai(&store, &["worktree", "list"])),
        format!(
            "{}{}{}",
            line(main_path, abbrev, "[main]"),
            line(aaa_path, "0000000", "[feature]"),
            line(zzz_path, abbrev, "(detached HEAD)"),
        )
    );

    let listed: serde_json::Value =
        serde_json::from_str(stdout(&sekai(&store, &["worktree", "list", "--json"]))).unwrap();
    assert_eq!(
        listed["result"],
        serde_json::json!([
            {"path": main_path, "commit": commit, "branch": "refs/heads/main", "main": true},
            {"path": aaa_path, "commit": null, "branch": "refs/heads/feature", "main": false},
            {"path": zzz_path, "commit": commit, "branch": null, "main": false},
        ])
    );

    // Porcelain with -z: every terminator, the record separator included,
    // is a NUL.
    let zeros = "0".repeat(64);
    assert_eq!(
        stdout(&sekai(&store, &["worktree", "list", "--porcelain", "-z"])),
        format!(
            "worktree {main_path}\0HEAD {commit}\0branch refs/heads/main\0\0\
             worktree {aaa_path}\0HEAD {zeros}\0branch refs/heads/feature\0\0\
             worktree {zzz_path}\0HEAD {commit}\0detached\0\0"
        )
    );
}

/// Opening through a store link resolves back to the same store, and the
/// main worktree stays the store root's row, not the link's.
#[tokio::test]
async fn listing_through_a_link_keeps_the_main_worktree_first() {
    let root = tempdir("worktree-link-open");
    let store = init(root.path());
    let commit = first_commit(&store, root.path());

    let world = root.path().join("world");
    let link = world.join(".sekai");
    let opened = LocalStore::open(&store).await.unwrap();
    opened
        .add_worktree(&link, Head::Branch(RefName::parse("feature").unwrap()))
        .await
        .unwrap();
    drop(opened);

    let listed: serde_json::Value =
        serde_json::from_str(stdout(&sekai(&link, &["worktree", "list", "--json"]))).unwrap();
    let main_path = std::path::absolute(&store).unwrap();
    assert_eq!(
        listed["result"][0],
        serde_json::json!({
            "path": main_path.to_str().unwrap(),
            "commit": commit,
            "branch": "refs/heads/main",
            "main": true,
        })
    );
    assert_eq!(listed["result"][1]["main"], false);
}

#[test]
fn a_missing_store_fails_with_an_error_envelope() {
    let root = tempdir("worktree-missing");
    let output = sekai(root.path().join("absent"), &["worktree", "list", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["command"], "worktree");
    assert_eq!(envelope["status"], "error");
}

/// Register a linked worktree at `<root>/world/.sekai` on `feature`,
/// leaving a `level.dat` behind so a removal can prove the world survives.
/// Returns the link path. The store handle is dropped so the binary can
/// take the session lock.
async fn linked_worktree(store: impl AsRef<Path>, root: impl AsRef<Path>) -> std::path::PathBuf {
    let world = root.as_ref().join("world");
    std::fs::create_dir_all(&world).unwrap();
    std::fs::write(world.join("level.dat"), "world data").unwrap();
    let link = world.join(".sekai");
    let opened = LocalStore::open(store).await.unwrap();
    opened
        .add_worktree(&link, Head::Branch(RefName::parse("feature").unwrap()))
        .await
        .unwrap();
    drop(opened);
    link
}

#[tokio::test]
async fn removes_a_linked_worktree_and_leaves_the_world() {
    let root = tempdir("worktree-remove");
    let store = init(root.path());
    let link = linked_worktree(&store, root.path()).await;
    let key = link_key(&link);

    assert_eq!(
        stdout(&sekai(
            &store,
            &["worktree", "remove", link.to_str().unwrap()]
        )),
        format!("removed worktree {key}\n")
    );
    assert!(!link.exists());
    // The world the worktree backed up is untouched.
    assert_eq!(
        std::fs::read_to_string(root.path().join("world/level.dat")).unwrap(),
        "world data"
    );
    // Only the store's own (main) worktree remains.
    let listed: serde_json::Value =
        serde_json::from_str(stdout(&sekai(&store, &["worktree", "list", "--json"]))).unwrap();
    let rows = listed["result"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["main"], true);
}

#[tokio::test]
async fn remove_reports_the_removed_path_as_json() {
    let root = tempdir("worktree-remove-json");
    let store = init(root.path());
    let link = linked_worktree(&store, root.path()).await;
    let key = link_key(&link);

    let envelope: serde_json::Value = serde_json::from_str(stdout(&sekai(
        &store,
        &["worktree", "remove", "--json", link.to_str().unwrap()],
    )))
    .unwrap();
    assert_eq!(envelope["command"], "worktree");
    assert_eq!(envelope["status"], "ok");
    assert_eq!(envelope["result"]["path"], key);
}

/// `--force` is inert (sekai worktrees carry no dirty or locked state) but
/// accepted, and repeatable as Git's force is.
#[tokio::test]
async fn remove_accepts_a_repeatable_force() {
    let root = tempdir("worktree-remove-force");
    let store = init(root.path());
    let link = linked_worktree(&store, root.path()).await;

    stdout(&sekai(
        &store,
        &["worktree", "remove", "-f", "-f", link.to_str().unwrap()],
    ));
    assert!(!link.exists());
}

#[test]
fn remove_refuses_the_main_worktree() {
    let root = tempdir("worktree-remove-main");
    let store = init(root.path());
    let output = sekai(&store, &["worktree", "remove", store.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("main worktree"), "got: {stderr}");
}

#[test]
fn remove_refuses_an_unregistered_location() {
    let root = tempdir("worktree-remove-unknown");
    let store = init(root.path());
    let output = sekai(
        &store,
        &[
            "worktree",
            "remove",
            root.path().join("nope").to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not a worktree"), "got: {stderr}");
}

#[test]
fn remove_refusal_is_an_error_envelope_in_json() {
    let root = tempdir("worktree-remove-main-json");
    let store = init(root.path());
    let output = sekai(
        &store,
        &["worktree", "remove", "--json", store.to_str().unwrap()],
    );
    assert_eq!(output.status.code(), Some(1));
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["command"], "worktree");
    assert_eq!(envelope["status"], "error");
    assert!(
        envelope["error"]
            .as_str()
            .unwrap()
            .contains("main worktree"),
        "got: {envelope}"
    );
}
