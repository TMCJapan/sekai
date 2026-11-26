//! Worktree subcommand execution, DTOs, and output rendering.
//!
//! Worktrees are a local-backend feature (ADR-0038), so listing and removal
//! go through the opened `LocalStore` rather than an app operation.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::Context as _;
use sekai_app::{CommitId, Ref, RefName, SekaiInstance};
use sekai_storage_local::{LocalStore, Worktree};
use sekai_util::Head;
use serde::Serialize;
use unicode_width::UnicodeWidthStr as _;

use crate::cli::{WorktreeCommand, WorktreeListArgs, WorktreeRemoveArgs};
use crate::envelope::envelope_ok;

pub async fn run(store: &str, action: &WorktreeCommand) -> anyhow::Result<()> {
    match action {
        WorktreeCommand::List(args) => list(store, args).await,
        WorktreeCommand::Remove(args) => remove(store, args).await,
    }
}

async fn list(store: &str, args: &WorktreeListArgs) -> anyhow::Result<()> {
    let instance = SekaiInstance::<LocalStore>::open(store).await?;
    let rows = listed(&instance)
        .await
        .with_context(|| format!("worktree list for {store} failed"))?;
    if args.json {
        let payload: Vec<WorktreeJson> = rows.iter().map(WorktreeJson::from).collect();
        println!("{}", envelope_ok("worktree", &payload)?);
    } else if args.porcelain {
        print!("{}", porcelain(&rows, args.z));
    } else {
        let commits = instance
            .list_commits()
            .await
            .with_context(|| format!("worktree list for {store} failed"))?;
        let ids: Vec<CommitId> = commits.iter().map(|commit| commit.id).collect();
        print!("{}", human(&rows, &ids));
    }
    Ok(())
}

/// Remove a linked worktree through the local backend (ADR-0038). The
/// refusals (main worktree, unregistered location) surface as Git words
/// them; `--force` is accepted but inert, since sekai worktrees carry no
/// dirty or locked state to override.
async fn remove(store: &str, args: &WorktreeRemoveArgs) -> anyhow::Result<()> {
    let instance = SekaiInstance::<LocalStore>::open(store).await?;
    let removed = instance.store().remove_worktree(&args.worktree).await?;
    // Worktree keys are UTF-8 by construction (`store_key`), so the lossy
    // conversion never replaces anything.
    let path = removed.to_string_lossy().into_owned();
    if args.json {
        println!("{}", envelope_ok("worktree", &WorktreeRemoveJson { path })?);
    } else {
        println!("removed worktree {path}");
    }
    Ok(())
}

/// A worktree with its HEAD resolved for display: the commit it names
/// (`None` while unborn), the branch it sits on (`None` when detached),
/// and whether it is the store's own (main) worktree.
struct Listed {
    path: String,
    commit: Option<CommitId>,
    branch: Option<RefName>,
    main: bool,
}

/// The store's worktrees with HEADs resolved, the main one first and the
/// rest in path order, as Git lists them. A HEAD branch with no ref row
/// is unborn, like Git's HEAD on a branch that does not exist yet.
async fn listed(instance: &SekaiInstance<LocalStore>) -> anyhow::Result<Vec<Listed>> {
    let store = instance.store();
    let worktrees = store.worktrees().await?;
    let branches: HashMap<RefName, Option<CommitId>> = instance
        .list_branches()
        .await?
        .into_iter()
        .map(|branch| (branch.name.name, branch.commit))
        .collect();
    let mut rows: Vec<Listed> = worktrees
        .iter()
        .map(|worktree| resolve(worktree, &branches, store.root()))
        .collect();
    // Rows arrive in path order; rotate the main one to the front without
    // disturbing the order of the rest.
    if let Some(pos) = rows.iter().position(|row| row.main) {
        rows[..=pos].rotate_right(1);
    }
    Ok(rows)
}

fn resolve(
    worktree: &Worktree,
    branches: &HashMap<RefName, Option<CommitId>>,
    root: impl AsRef<Path>,
) -> Listed {
    let (commit, branch) = match &worktree.head {
        Head::Branch(name) => (branches.get(name).copied().flatten(), Some(name.clone())),
        Head::Detached(id) => (Some(*id), None),
    };
    Listed {
        // Worktree keys are UTF-8 by construction (`store_key`), so the
        // lossy conversion never replaces anything.
        path: worktree.path.to_string_lossy().into_owned(),
        commit,
        branch,
        main: worktree.path == root.as_ref(),
    }
}

/// Git's `DEFAULT_ABBREV`: the shortest commit prefix shown before
/// uniqueness forces a longer one.
const MIN_ABBREV: usize = 7;

/// One line per worktree, as Git prints them: the path padded toward
/// the widest path's byte length plus two spaces at the widest (Git
/// pads by display width, so wide glyphs take fewer spaces), then each
/// HEAD at its own shortest-unique abbreviation padded to the widest
/// one plus a space, then `[branch]` or `(detached HEAD)`. An unborn
/// HEAD shows zeros at the minimum width, like Git's null object ID.
fn human(rows: &[Listed], commits: &[CommitId]) -> String {
    let hexes: Vec<String> = commits.iter().map(|commit| commit.hex_string()).collect();
    let lens: Vec<usize> = rows
        .iter()
        .map(|row| row.commit.map_or(MIN_ABBREV, |id| unique_len(id, &hexes)))
        .collect();
    let path_maxlen = rows.iter().map(|row| row.path.len()).max().unwrap_or(0);
    let abbrev_len = lens.iter().copied().max().unwrap_or(MIN_ABBREV);
    let mut out = String::new();
    for (row, &len) in rows.iter().zip(&lens) {
        let hex = head_hex(row.commit, len);
        let suffix = row
            .branch
            .as_ref()
            .map_or_else(|| "(detached HEAD)".to_owned(), |name| format!("[{name}]"));
        let pad = path_maxlen + 1 - row.path.width();
        let _ = writeln!(out, "{}{:pad$} {:<abbrev_len$} {suffix}", row.path, "", hex);
    }
    out
}

/// The shortest hex prefix of `id` that no other store commit shares,
/// at least `MIN_ABBREV` chars, as Git's `find_unique_abbrev` returns.
fn unique_len(id: CommitId, hexes: &[String]) -> usize {
    let hex = id.hex_string();
    let mut len = MIN_ABBREV;
    while len < hex.len()
        && hexes
            .iter()
            .any(|other| other != &hex && other.starts_with(&hex[..len]))
    {
        len += 1;
    }
    len
}

/// The first `len` hex chars of a HEAD commit, or zeros while unborn.
fn head_hex(commit: Option<CommitId>, len: usize) -> String {
    commit.map_or_else(|| "0".repeat(len), |id| id.hex_string()[..len].to_owned())
}

/// Git's porcelain format: a `worktree`/`HEAD`/`branch` (or `detached`)
/// record block per worktree, blank-line separated. With `z`, every line
/// terminator - the separator included - is a NUL instead. An unborn HEAD
/// carries the all-zero ID, as Git's null object ID does.
fn porcelain(rows: &[Listed], z: bool) -> String {
    let term = if z { '\0' } else { '\n' };
    let mut out = String::new();
    for row in rows {
        let hex = head_hex(row.commit, 64);
        let _ = write!(out, "worktree {}{term}", row.path);
        let _ = write!(out, "HEAD {hex}{term}");
        match &row.branch {
            Some(name) => {
                let full = Ref::branch(name.clone()).full_name();
                let _ = write!(out, "branch {full}{term}");
            }
            None => {
                let _ = write!(out, "detached{term}");
            }
        }
        out.push(term);
    }
    out
}

/// A listed worktree: its absolute store location, its HEAD commit
/// (`null` while unborn), the full refname it sits on (`null` when
/// detached), and whether it is the store's own (main) worktree.
#[derive(Serialize)]
struct WorktreeJson {
    path: String,
    commit: Option<String>,
    branch: Option<String>,
    main: bool,
}

impl From<&Listed> for WorktreeJson {
    fn from(row: &Listed) -> Self {
        Self {
            path: row.path.clone(),
            commit: row.commit.map(|commit| commit.to_string()),
            branch: row
                .branch
                .as_ref()
                .map(|name| Ref::branch(name.clone()).full_name()),
            main: row.main,
        }
    }
}

/// A removed worktree: the absolute store location it was keyed by, the
/// identity `worktree list` reports.
#[derive(Serialize)]
struct WorktreeRemoveJson {
    path: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, commit: Option<CommitId>, branch: Option<&str>, main: bool) -> Listed {
        Listed {
            path: path.to_owned(),
            commit,
            branch: branch.map(|name| RefName::parse(name).unwrap()),
            main,
        }
    }

    #[test]
    fn human_pads_columns_like_git() {
        let commit = CommitId([0xAB; 32]);
        let rows = [
            row("/store", Some(commit), Some("main"), true),
            row("/worlds/linked", Some(commit), Some("feature/x"), false),
        ];
        // Git pads toward the widest path's byte length (14) plus two
        // spaces at the widest: "/store" (6) gets 10, the widest 2.
        // The abbrev column is 7 wide plus one space.
        let hex = &commit.hex_string()[..7];
        assert_eq!(
            human(&rows, &[commit]),
            format!(
                "/store{:9} {hex} [main]\n/worlds/linked{:1} {hex} [feature/x]\n",
                "", ""
            )
        );
    }

    #[test]
    fn human_marks_detached_and_unborn_heads() {
        let commit = CommitId([0x12; 32]);
        let rows = [
            row("/a", None, Some("main"), true),
            row("/b", Some(commit), None, false),
        ];
        let hex = &commit.hex_string()[..7];
        assert_eq!(
            human(&rows, &[commit]),
            format!("/a  0000000 [main]\n/b  {hex} (detached HEAD)\n")
        );
    }

    #[test]
    fn abbrev_grows_until_unique() {
        // The two IDs share their first 7 hex chars (1234567), so each
        // needs 8, as Git's find_unique_abbrev returns.
        let mut first = [0x55; 32];
        let mut second = [0x66; 32];
        first[..4].copy_from_slice(&[0x12, 0x34, 0x56, 0x78]);
        second[..4].copy_from_slice(&[0x12, 0x34, 0x56, 0x79]);
        let (first, second) = (CommitId(first), CommitId(second));
        let hexes = [first.hex_string(), second.hex_string()];
        assert_eq!(unique_len(first, &hexes), 8);
        assert_eq!(unique_len(second, &hexes), 8);
        let rows = [
            row("/a", Some(first), Some("main"), true),
            row("/b", Some(second), None, false),
        ];
        let first_hex = &hexes[0][..8];
        let second_hex = &hexes[1][..8];
        assert_eq!(
            human(&rows, &[first, second]),
            format!("/a  {first_hex} [main]\n/b  {second_hex} (detached HEAD)\n")
        );
    }

    #[test]
    fn abbrev_pads_shorter_heads_and_unborn_zeros() {
        // Git shows each HEAD at its own shortest-unique length, padded
        // to the widest (8 here): the 7-char HEAD and the unborn zeros
        // gain a pad space, the 8-char HEAD does not.
        let mut long = [0x55; 32];
        long[..4].copy_from_slice(&[0x12, 0x34, 0x56, 0x78]);
        let mut other = [0x66; 32];
        other[..4].copy_from_slice(&[0x12, 0x34, 0x56, 0x79]);
        let (long, other, short) = (CommitId(long), CommitId(other), CommitId([0xAA; 32]));
        let rows = [
            row("/a", Some(short), Some("main"), true),
            row("/b", Some(long), None, false),
            row("/c", None, Some("feature"), false),
        ];
        let long_hex = &long.hex_string()[..8];
        let short_hex = &short.hex_string()[..7];
        assert_eq!(
            human(&rows, &[long, other, short]),
            format!(
                "/a  {short_hex}  [main]\n/b  {long_hex} (detached HEAD)\n/c  0000000  [feature]\n"
            )
        );
    }

    #[test]
    fn human_pads_wide_glyph_paths_by_display_width() {
        // Git measures the column by byte length (7 for "/世界") but
        // pads by display width (5): both rows land on the same column.
        let commit = CommitId([0xCD; 32]);
        let rows = [
            row("/世界", Some(commit), Some("main"), true),
            row("/abcde", Some(commit), Some("dev"), false),
        ];
        let hex = &commit.hex_string()[..7];
        assert_eq!(
            human(&rows, &[commit]),
            format!("/世界    {hex} [main]\n/abcde   {hex} [dev]\n")
        );
    }

    #[test]
    fn porcelain_blocks_match_git() {
        let commit = CommitId([0x0A; 32]);
        let rows = [
            row("/store", None, Some("main"), true),
            row("/link", Some(commit), None, false),
        ];
        let hex = commit.hex_string();
        assert_eq!(
            porcelain(&rows, false),
            format!(
                "worktree /store\nHEAD {zeros}\nbranch refs/heads/main\n\n\
                 worktree /link\nHEAD {hex}\ndetached\n\n",
                zeros = "0".repeat(64),
            )
        );
        assert_eq!(
            porcelain(&rows, true),
            format!(
                "worktree /store\0HEAD {zeros}\0branch refs/heads/main\0\0\
                 worktree /link\0HEAD {hex}\0detached\0\0",
                zeros = "0".repeat(64),
            )
        );
    }

    #[test]
    fn renders_worktree_json() {
        let commit = CommitId([3; 32]);
        let rows = [
            row("/store", None, Some("main"), true),
            row("/link", Some(commit), None, false),
        ];
        let payload: Vec<WorktreeJson> = rows.iter().map(WorktreeJson::from).collect();
        assert_eq!(
            serde_json::to_string(&payload).unwrap(),
            format!(
                r#"[{{"path":"/store","commit":null,"branch":"refs/heads/main","main":true}},{{"path":"/link","commit":"{commit}","branch":null,"main":false}}]"#
            )
        );
    }
}
