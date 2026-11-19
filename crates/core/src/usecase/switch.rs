//! Switching HEAD (ADR-0035 *Resolution*, ADR-0036 *checkout*).
//!
//! [`resolve_target`] decides what an argument that may name a branch
//! selects, and [`switch_head`] moves HEAD there, as Git's
//! `switch_branches` does with its `branch_info`. [`resolve_worktree_head`]
//! makes the same decision for the HEAD of a new worktree.

use alloc::string::String;

use sekai_storage::MetaStore;
use sekai_util::{
    CommitId, Head, HeadState, Ref, RefName, ReflogNote, Revision, RevisionBase, WorktreeName,
};

use super::head::{born_head, head_label, head_state};
use super::messages::checkout_message;
use super::resolve::{ResolveError, resolve_commit_ref};
use super::revision::resolve_revision;

/// What a `checkout` target or a `worktree add` `<commit-ish>` selects
/// (ADR-0035 *Resolution*, ADR-0036 *checkout*).
///
/// It holds a commit and what happens to HEAD, as Git's `branch_info`
/// holds a commit and the branch path that decides between attaching and
/// detaching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Commit the target stands at.
    pub commit: CommitId,
    /// What switching to the target does to HEAD.
    pub head: HeadMove,
}

/// What switching to a [`Target`] does to HEAD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadMove {
    /// `HEAD` or `@` alone: HEAD stays as it is, and nothing is logged.
    Keep,
    /// A branch: HEAD attaches to it.
    Attach(RefName),
    /// Anything else: HEAD detaches at the target's commit.
    Detach {
        /// How the reflog names the target: the revision as written.
        label: String,
    },
}

/// Resolve a `checkout` target or a `worktree add` `<commit-ish>`: an
/// argument that may name a branch.
///
/// A plain name names a branch when `refs/heads/<name>` exists; a
/// qualified form such as `heads/x` does not, as in Git. A same-named
/// tag does not make the branch rule ambiguous; the branch wins,
/// silently **(deviation)**. `HEAD` and `@` keep HEAD as it is.
/// Anything else detaches HEAD.
pub async fn resolve_target<M: MetaStore>(
    meta: &M,
    worktree: Option<&WorktreeName>,
    revision: &Revision,
) -> Result<Target, ResolveError<M::Error>> {
    match (revision.base(), revision.steps().is_empty()) {
        (RevisionBase::Head, true) => Ok(Target {
            commit: born_head(meta, worktree).await?,
            head: HeadMove::Keep,
        }),
        (RevisionBase::Name(name), true) => branch_or_commit(meta, name).await,
        _ => Ok(Target {
            commit: resolve_revision(meta, worktree, revision).await?,
            head: HeadMove::Detach {
                label: String::from(revision.as_str()),
            },
        }),
    }
}

/// What `worktree add` starts the new worktree's HEAD from (ADR-0035
/// *`worktree add`*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeStart<'a> {
    /// `<commit-ish>` as given.
    CommitIsh(&'a Revision),
    /// `<commit-ish>` omitted: the branch named after the basename of the
    /// new worktree's path.
    Basename(&'a RefName),
}

/// The HEAD a new worktree starts with, and the commit it stands at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewWorktreeHead {
    /// Commit the new worktree is checked out at.
    pub commit: CommitId,
    /// What the new worktree's HEAD stores.
    pub head: Head,
    /// Whether `head`'s branch does not exist yet and has to be created
    /// at `commit`.
    pub creates_branch: bool,
}

/// Decide the HEAD of a worktree that `worktree add` copies from
/// `source` (ADR-0035 *`worktree add`*).
///
/// A `<commit-ish>` resolves as [`resolve_target`] does, except that
/// `HEAD` and `@` detach too, as `git worktree add <path> HEAD` does: a
/// branch attaches, and anything else detaches. Without one, the
/// basename's branch attaches when it exists; otherwise it is created at
/// `source`'s HEAD, which fails while that HEAD is unborn, where Git
/// would infer `--orphan`.
pub async fn resolve_worktree_head<M: MetaStore>(
    meta: &M,
    source: Option<&WorktreeName>,
    start: WorktreeStart<'_>,
) -> Result<NewWorktreeHead, ResolveError<M::Error>> {
    let (commit, head, creates_branch) = match start {
        WorktreeStart::CommitIsh(revision) => {
            let target = resolve_target(meta, source, revision).await?;
            let head = match target.head {
                HeadMove::Attach(name) => Head::Branch(name),
                HeadMove::Keep | HeadMove::Detach { .. } => Head::Detached(target.commit),
            };
            (target.commit, head, false)
        }
        WorktreeStart::Basename(name) => match meta
            .lookup_ref(&Ref::branch(name.clone()))
            .await
            .map_err(ResolveError::Meta)?
        {
            Some(record) => (record.commit, Head::Branch(name.clone()), false),
            None => (
                born_head(meta, source).await?,
                Head::Branch(name.clone()),
                true,
            ),
        },
    };
    Ok(NewWorktreeHead {
        commit,
        head,
        creates_branch,
    })
}

/// `name` as a branch when `refs/heads/<name>` exists, and as a
/// detached commit otherwise.
async fn branch_or_commit<M: MetaStore>(
    meta: &M,
    name: &RefName,
) -> Result<Target, ResolveError<M::Error>> {
    let input = name.as_str();
    if let Some(record) = meta
        .lookup_ref(&Ref::branch(name.clone()))
        .await
        .map_err(ResolveError::Meta)?
    {
        // The branch-first rule is deterministic, so it stays silent
        // **(deviation)**: a same-named tag does not make the name
        // ambiguous for a checkout target.
        return Ok(Target {
            commit: record.commit,
            head: HeadMove::Attach(name.clone()),
        });
    }
    Ok(Target {
        commit: resolve_commit_ref(meta, input).await?,
        head: HeadMove::Detach {
            label: String::from(input),
        },
    })
}

/// Point `worktree`'s HEAD at `target`, logging Git's `checkout: moving
/// from <a> to <b>` (ADR-0036 *checkout*), where `<a>` is HEAD as the
/// store holds it before the switch.
///
/// [`HeadMove::Keep`] leaves HEAD as it is and logs nothing, as
/// `git checkout HEAD` does; a detach at the commit HEAD already
/// holds logs nothing, as `git checkout --detach HEAD` does. An
/// unknown worktree is [`ResolveError::UnknownWorktree`]; a branch
/// checked out elsewhere is refused by the store.
pub async fn switch_head<M: MetaStore>(
    meta: &mut M,
    worktree: &WorktreeName,
    target: &Target,
    at_ms: u64,
) -> Result<(), ResolveError<M::Error>> {
    // The store reads an unknown worktree's HEAD log as empty, so the
    // worktree must be proven to exist even for a no-op switch.
    let (_, from) = head_state(meta, Some(worktree)).await?;
    let (head, to) = match &target.head {
        HeadMove::Keep => return Ok(()),
        HeadMove::Attach(name) => (Head::Branch(name.clone()), name.as_str()),
        HeadMove::Detach { label } => {
            if matches!(&from, HeadState::Detached { commit } if *commit == target.commit) {
                return Ok(());
            }
            (Head::Detached(target.commit), label.as_str())
        }
    };
    let message = checkout_message(&head_label(&from), to);
    meta.set_head(worktree, &head, ReflogNote::new(&message, at_ms))
        .await
        .map_err(ResolveError::Meta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{
        MemMeta, add_ref, block_on, linear_history, main_worktree, ref_name, worktree_name,
    };
    use alloc::format;
    use alloc::string::ToString;
    use alloc::vec::Vec;
    use sekai_util::HeadState;

    fn rev(text: &str) -> Revision {
        Revision::parse(text).unwrap()
    }

    fn attach(name: &str, commit: CommitId) -> Target {
        Target {
            commit,
            head: HeadMove::Attach(ref_name(name)),
        }
    }

    fn detach(commit: CommitId, label: &str) -> Target {
        Target {
            commit,
            head: HeadMove::Detach {
                label: label.into(),
            },
        }
    }

    fn keep(commit: CommitId) -> Target {
        Target {
            commit,
            head: HeadMove::Keep,
        }
    }

    #[test]
    fn a_target_prefers_the_branch() {
        let (mut meta, [a, b, c]) = linear_history();
        add_ref(&mut meta, &Ref::tag(ref_name("x")), a);
        add_ref(&mut meta, &Ref::branch(ref_name("x")), b);
        add_ref(&mut meta, &Ref::tag(ref_name("v1")), a);
        let target =
            |text: &str| block_on(resolve_target(&meta, Some(&main_worktree()), &rev(text)));

        // A branch wins over a tag of the same name, silently (deviation).
        assert_eq!(target("x"), Ok(attach("x", b)));
        // A qualified form never names a branch, as in Git.
        assert_eq!(target("heads/x"), Ok(detach(b, "heads/x")));
        assert_eq!(target("v1"), Ok(detach(a, "v1")));
        assert_eq!(target("main~1"), Ok(detach(b, "main~1")));
        assert_eq!(target("HEAD"), Ok(keep(c)));
        assert_eq!(target("@"), Ok(keep(c)));
    }

    #[test]
    fn a_qualified_branch_name_wins_deterministically() {
        // `git checkout refs/heads/y` with branches `y` and `refs/heads/y`
        // attaches to the latter and warns (git 2.53); sekai attaches
        // without warning (deviation).
        let (mut meta, [a, b, _]) = linear_history();
        add_ref(&mut meta, &Ref::branch(ref_name("y")), a);
        add_ref(&mut meta, &Ref::branch(ref_name("refs/heads/y")), b);
        assert_eq!(
            block_on(resolve_target(&meta, None, &rev("refs/heads/y"))),
            Ok(attach("refs/heads/y", b))
        );
    }

    #[test]
    fn a_branch_named_like_a_commit_id_still_names_the_branch() {
        // Git's checkout looks for `refs/heads/<arg>` before anything else.
        let (mut meta, [a, b, _]) = linear_history();
        let hex = a.to_string();
        add_ref(&mut meta, &Ref::branch(ref_name(&hex)), b);
        assert_eq!(
            block_on(resolve_target(&meta, None, &rev(&hex))),
            Ok(attach(&hex, b))
        );
    }

    fn new_head(commit: CommitId, head: Head, creates_branch: bool) -> NewWorktreeHead {
        NewWorktreeHead {
            commit,
            head,
            creates_branch,
        }
    }

    #[test]
    fn a_commit_ish_attaches_only_to_a_branch() {
        let (mut meta, [a, b, c]) = linear_history();
        add_ref(&mut meta, &Ref::branch(ref_name("dev")), b);
        add_ref(&mut meta, &Ref::tag(ref_name("v1")), a);
        let main = main_worktree();
        let start = |text: &str| {
            let revision = rev(text);
            block_on(resolve_worktree_head(
                &meta,
                Some(&main),
                WorktreeStart::CommitIsh(&revision),
            ))
        };

        assert_eq!(
            start("dev"),
            Ok(new_head(b, Head::Branch(ref_name("dev")), false))
        );
        assert_eq!(start("v1"), Ok(new_head(a, Head::Detached(a), false)));
        // `HEAD` detaches, as `git worktree add <path> HEAD` does.
        assert_eq!(start("HEAD"), Ok(new_head(c, Head::Detached(c), false)));
        assert_eq!(start("@"), Ok(new_head(c, Head::Detached(c), false)));
    }

    #[test]
    fn a_basename_takes_its_branch_or_creates_it_at_head() {
        let (mut meta, [_, b, c]) = linear_history();
        add_ref(&mut meta, &Ref::branch(ref_name("dev")), b);
        let main = main_worktree();
        let start = |meta: &MemMeta, name: &str| {
            let name = ref_name(name);
            block_on(resolve_worktree_head(
                meta,
                Some(&main),
                WorktreeStart::Basename(&name),
            ))
        };

        assert_eq!(
            start(&meta, "dev"),
            Ok(new_head(b, Head::Branch(ref_name("dev")), false))
        );
        assert_eq!(
            start(&meta, "feature"),
            Ok(new_head(c, Head::Branch(ref_name("feature")), true))
        );

        // An unborn HEAD fails only when the branch has to be created
        // from it; Git would infer `--orphan` there.
        let mut unborn = MemMeta::default();
        unborn.with_main();
        let commit = unborn.create_commit(100, &[]).unwrap();
        add_ref(&mut unborn, &Ref::branch(ref_name("dev")), commit);
        assert_eq!(
            start(&unborn, "dev"),
            Ok(new_head(commit, Head::Branch(ref_name("dev")), false))
        );
        assert_eq!(
            start(&unborn, "feature"),
            Err(ResolveError::UnbornHead {
                branch: ref_name("main")
            })
        );
    }

    #[test]
    fn switching_an_unknown_worktree_fails() {
        let (mut meta, [a, ..]) = linear_history();
        let ghost = worktree_name("ghost");
        for target in [keep(a), detach(a, "main~2")] {
            assert_eq!(
                block_on(switch_head(&mut meta, &ghost, &target, 0)),
                Err(ResolveError::UnknownWorktree {
                    name: ghost.clone()
                }),
                "{target:?}"
            );
        }
    }

    #[test]
    fn a_detach_at_the_same_commit_logs_nothing() {
        let mut meta = MemMeta::default();
        meta.with_main();
        let a = block_on(meta.apply_main(100, &[], &[])).unwrap().id;
        let worktree = main_worktree();
        for _ in 0..2 {
            block_on(switch_head(&mut meta, &worktree, &detach(a, "main~0"), 300)).unwrap();
        }

        let mut messages = Vec::new();
        block_on(meta.visit_head_log(&worktree, |entry| {
            messages.push(entry.message.clone());
            true
        }))
        .unwrap();
        // The second detach changed nothing, so only the first one logs.
        assert_eq!(
            messages,
            ["checkout: moving from main to main~0", "commit: "].map(String::from)
        );
    }

    #[test]
    fn switching_logs_gits_checkout_messages() {
        let mut meta = MemMeta::default();
        meta.with_main();
        let a = block_on(meta.apply_main(100, &[], &[])).unwrap().id;
        let dev = ref_name("dev");
        add_ref(&mut meta, &Ref::branch(dev.clone()), a);
        let worktree = main_worktree();
        let head = |meta: &MemMeta| block_on(meta.head(&worktree)).unwrap().unwrap();
        let switch = |meta: &mut MemMeta, target: &Target, at_ms| {
            block_on(switch_head(meta, &worktree, target, at_ms)).unwrap();
        };

        switch(&mut meta, &keep(a), 200);
        switch(&mut meta, &detach(a, "main~0"), 300);
        switch(&mut meta, &attach("dev", a), 400);
        assert_eq!(
            head(&meta),
            HeadState::Attached {
                branch: dev,
                commit: a
            }
        );

        let mut messages = Vec::new();
        block_on(meta.visit_head_log(&worktree, |entry| {
            messages.push(entry.message.clone());
            true
        }))
        .unwrap();
        let a_hex = a.to_string();
        assert_eq!(
            messages,
            [
                format!("checkout: moving from {a_hex} to dev"),
                "checkout: moving from main to main~0".into(),
                "commit: ".into(),
            ]
        );
    }
}
