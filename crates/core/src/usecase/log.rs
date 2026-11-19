//! The history `log` walks and where it starts (ADR-0036 *log*).

use alloc::collections::{BTreeSet, VecDeque};
use alloc::vec::Vec;

use sekai_storage::MetaStore;
use sekai_util::{Commit, CommitId, Head, Revision, RevisionRange, Side, WorktreeName};

use super::graph::{CommitError, lookup_record, reachable_from};
use super::refs::list_refs;
use super::resolve::ResolveError;
use super::revision::resolve_revision;

/// Commits reachable from `bounds.include` but not from
/// `bounds.exclude`, newest first, in Git's default order.
///
/// The walk follows Git's `get_revision_1`: it keeps the commits it has
/// reached but not listed in a queue ordered by date with ties in reach
/// order, lists the front one, and queues that commit's parents. A
/// parent is only reached through a listed child, so it never comes
/// before that child, even when its time is later. Commits with the
/// same time keep the order the walk reached them in, and the starting
/// points the order given.
///
/// Unlike Git's `limit_list`, which marks excluded commits while it
/// walks and stops early, the walk collects every commit the exclusions
/// reach first, so a clock that stepped back never lets one through.
pub async fn walk_log<M: MetaStore>(
    meta: &M,
    bounds: &LogBounds,
) -> Result<Vec<Commit>, CommitError<M::Error>> {
    let hidden = reachable_from(meta, &bounds.exclude).await?;
    let mut queue = VecDeque::new();
    let mut seen = BTreeSet::new();
    for &id in &bounds.include {
        if !hidden.contains(&id) && seen.insert(id) {
            insert_by_date(&mut queue, lookup_record(meta, id).await?);
        }
    }
    let mut out = Vec::new();
    while let Some(commit) = queue.pop_front() {
        for &parent in &commit.parents {
            if !hidden.contains(&parent) && seen.insert(parent) {
                insert_by_date(&mut queue, lookup_record(meta, parent).await?);
            }
        }
        out.push(commit);
    }
    Ok(out)
}

/// Insert `commit` into `queue`, kept newest first, after every commit of
/// the same time, as Git's `commit_list_insert_by_date` does.
fn insert_by_date(queue: &mut VecDeque<Commit>, commit: Commit) {
    let at = queue.partition_point(|queued| queued.created_at_ms >= commit.created_at_ms);
    queue.insert(at, commit);
}

/// Where `log --all` starts: every ref, then every detached HEAD.
///
/// Git's `--all` takes the HEADs of all worktrees too. An attached HEAD
/// stands where its branch does, and an unborn one stands nowhere, so
/// only a detached HEAD adds a starting point.
async fn all_starting_points<M: MetaStore>(meta: &M) -> Result<Vec<CommitId>, M::Error> {
    let mut starts: Vec<CommitId> = list_refs(meta).await?.iter().map(|r| r.commit).collect();
    meta.visit_worktrees(|worktree| {
        if let Head::Detached(commit) = worktree.head {
            starts.push(commit);
        }
        true
    })
    .await?;
    Ok(starts)
}

/// The starting points and exclusions `log`'s revision arguments and
/// `--all` name.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LogBounds {
    /// Commits the walk starts from; each commit appears once.
    pub include: Vec<CommitId>,
    /// Commits whose ancestry the walk leaves out.
    pub exclude: Vec<CommitId>,
}

/// Resolve `log`'s revision arguments and `--all`, as Git's
/// `setup_revisions` does.
///
/// Each revision resolves to a commit on its [`Side`], in argument order.
/// An ambiguous revision is an error. `--all` adds every ref and
/// every detached HEAD; its starting points follow the revisions', since
/// the order of the arguments is not kept. HEAD is added only when
/// neither is given, so `--all` alone works with an unborn HEAD and
/// outside every worktree, and exclusions alone start from nothing.
pub async fn resolve_log_bounds<M: MetaStore>(
    meta: &M,
    worktree: Option<&WorktreeName>,
    ranges: &[RevisionRange],
    all: bool,
) -> Result<LogBounds, ResolveError<M::Error>> {
    let mut bounds = LogBounds::default();
    for side in ranges.iter().flat_map(RevisionRange::sides) {
        let (revision, commits) = match side {
            Side::Include(revision) => (revision, &mut bounds.include),
            Side::Exclude(revision) => (revision, &mut bounds.exclude),
        };
        let id = resolve_revision(meta, worktree, revision).await?;
        if !commits.contains(&id) {
            commits.push(id);
        }
    }
    if all {
        let starts = all_starting_points(meta)
            .await
            .map_err(ResolveError::Meta)?;
        for start in starts {
            if !bounds.include.contains(&start) {
                bounds.include.push(start);
            }
        }
    }
    if ranges.is_empty() && !all {
        let head = resolve_revision(meta, worktree, &Revision::head()).await?;
        bounds.include.push(head);
    }
    Ok(bounds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{
        MemError, MemMeta, add_ref, block_on, fork, main_worktree, ref_name, worktree_name,
    };
    use alloc::string::String;
    use sekai_util::{Ref, ReflogNote, Worktree};

    fn ids(commits: &[Commit]) -> Vec<CommitId> {
        commits.iter().map(|commit| commit.id).collect()
    }

    fn walk(meta: &MemMeta, include: &[CommitId], exclude: &[CommitId]) -> Vec<CommitId> {
        let bounds = LogBounds {
            include: include.to_vec(),
            exclude: exclude.to_vec(),
        };
        ids(&block_on(walk_log(meta, &bounds)).unwrap())
    }

    #[test]
    fn walks_newest_first_across_lines() {
        let (meta, [a, b, c]) = fork();
        assert_eq!(walk(&meta, &[c, b], &[]), [b, c, a]);
        // `c..b`: `a` is reachable from `c`, so only `b` is left.
        assert_eq!(walk(&meta, &[b], &[c]), [b]);
        assert_eq!(walk(&meta, &[b], &[b]), [] as [CommitId; 0]);
    }

    #[test]
    fn a_parent_never_precedes_its_child() {
        // `b` is older than its parent `a`, as a clock step back would
        // leave it; the walk still reaches `a` through `b`.
        let mut meta = MemMeta::default();
        let a = meta.create_commit(300, &[]).unwrap();
        let b = meta.create_commit(100, &[a]).unwrap();
        let c = meta.create_commit(200, &[]).unwrap();
        assert_eq!(walk(&meta, &[b, c], &[]), [c, b, a]);
    }

    #[test]
    fn ties_keep_the_order_the_walk_reached_them() {
        // Commits are content-addressed, so `z` needs its own parent `w`
        // to differ from `y` at the same time.
        let mut meta = MemMeta::default();
        let x = meta.create_commit(100, &[]).unwrap();
        let y = meta.create_commit(100, &[x]).unwrap();
        let w = meta.create_commit(50, &[]).unwrap();
        let z = meta.create_commit(100, &[w]).unwrap();
        assert_eq!(walk(&meta, &[z, y], &[]), [z, y, x, w]);
        assert_eq!(walk(&meta, &[y, z], &[]), [y, z, x, w]);
    }

    #[test]
    fn all_starting_points_take_refs_and_detached_heads() {
        let (mut meta, [a, b, c]) = fork();
        meta.with_main();
        let main = Ref::branch(ref_name("main"));
        add_ref(&mut meta, &main, b);
        let tag = Ref::tag(ref_name("v1"));
        add_ref(&mut meta, &tag, a);
        let linked = Worktree {
            name: worktree_name("linked"),
            path: Some("/worlds/linked".into()),
            head: Head::Detached(c),
        };
        block_on(meta.add_worktree(&linked, ReflogNote::new("", 0))).unwrap();

        // Refs in refname order, then detached HEADs; `main`'s HEAD stands
        // where the branch does.
        assert_eq!(
            block_on(all_starting_points(&meta)),
            Ok(alloc::vec![b, a, c])
        );
    }

    #[test]
    fn bounds_default_to_head_and_split_ranges() {
        let mut meta = MemMeta::default();
        meta.with_main();
        let a = block_on(meta.apply_main(100, &[], &[])).unwrap().id;
        let b = block_on(meta.apply_main(200, &[a], &[])).unwrap().id;
        let bounds = |args: &[&str]| {
            block_on(resolve_log_bounds(
                &meta,
                Some(&main_worktree()),
                &ranges(args),
                false,
            ))
        };
        let expect = |include: &[CommitId], exclude: &[CommitId]| {
            Ok::<_, ResolveError<MemError>>(LogBounds {
                include: include.to_vec(),
                exclude: exclude.to_vec(),
            })
        };

        assert_eq!(bounds(&[]), expect(&[b], &[]));
        assert_eq!(bounds(&["HEAD~1..main"]), expect(&[b], &[a]));
        assert_eq!(bounds(&["HEAD~1.."]), expect(&[b], &[a]));
        assert_eq!(bounds(&["^HEAD~1"]), expect(&[], &[a]));
        assert_eq!(bounds(&["main", "^HEAD~1"]), expect(&[b], &[a]));
        assert_eq!(
            block_on(resolve_log_bounds(&meta, None, &[], false)),
            Err(ResolveError::NoWorktree)
        );
    }

    #[test]
    fn the_same_commit_is_kept_once() {
        let mut meta = MemMeta::default();
        meta.with_main();
        let a = block_on(meta.apply_main(100, &[], &[])).unwrap().id;
        let b = block_on(meta.apply_main(200, &[a], &[])).unwrap().id;
        let bounds = |args: &[&str]| {
            block_on(resolve_log_bounds(
                &meta,
                Some(&main_worktree()),
                &ranges(args),
                false,
            ))
        };
        let expect = |include: &[CommitId], exclude: &[CommitId]| {
            Ok::<_, ResolveError<MemError>>(LogBounds {
                include: include.to_vec(),
                exclude: exclude.to_vec(),
            })
        };

        assert_eq!(bounds(&["main", "main"]), expect(&[b], &[]));
        assert_eq!(bounds(&["HEAD", "HEAD~0"]), expect(&[b], &[]));
        assert_eq!(bounds(&["^HEAD~1", "^HEAD~1"]), expect(&[], &[a]));
    }

    #[test]
    fn all_needs_no_head_and_joins_the_revisions() {
        // HEAD is unborn on `main`, which `git log` refuses and
        // `git log --all` does not (git 2.53).
        let (mut meta, [a, _, c]) = fork();
        meta.with_main();
        let side = Ref::branch(ref_name("side"));
        add_ref(&mut meta, &side, c);
        let tag = Ref::tag(ref_name("v1"));
        add_ref(&mut meta, &tag, a);
        let bounds = |worktree: Option<&WorktreeName>, args: &[&str], all| {
            block_on(resolve_log_bounds(&meta, worktree, &ranges(args), all))
        };
        let expect = |include: &[CommitId], exclude: &[CommitId]| {
            Ok::<_, ResolveError<MemError>>(LogBounds {
                include: include.to_vec(),
                exclude: exclude.to_vec(),
            })
        };
        let main = main_worktree();

        assert_eq!(
            bounds(Some(&main), &[], false),
            Err(ResolveError::UnbornHead {
                branch: ref_name("main")
            })
        );
        assert_eq!(bounds(Some(&main), &[], true), expect(&[c, a], &[]));
        assert_eq!(bounds(None, &[], true), expect(&[c, a], &[]));
        assert_eq!(bounds(None, &["^v1"], true), expect(&[c, a], &[a]));
        assert_eq!(bounds(None, &["v1"], true), expect(&[a, c], &[]));
    }

    #[test]
    fn an_ambiguous_revision_is_an_error() {
        // A tag and a branch named `x` make the spelling ambiguous;
        // bounds fail rather than guess (deviation).
        let (mut meta, [a, b, _]) = fork();
        add_ref(&mut meta, &Ref::tag(ref_name("x")), a);
        add_ref(&mut meta, &Ref::branch(ref_name("x")), b);
        let bounds =
            |args: &[&str]| block_on(resolve_log_bounds(&meta, None, &ranges(args), false));
        let ambiguous_x = Err(ResolveError::AmbiguousRef {
            input: String::from("x"),
            matching: alloc::vec![String::from("refs/tags/x"), String::from("refs/heads/x"),],
        });
        assert_eq!(bounds(&["x"]), ambiguous_x);
        // `x` fails the same way behind an exclusion.
        assert_eq!(bounds(&["x..nope"]), ambiguous_x);
    }

    fn ranges(args: &[&str]) -> Vec<RevisionRange> {
        args.iter()
            .map(|arg| RevisionRange::parse(arg).unwrap())
            .collect()
    }
}
