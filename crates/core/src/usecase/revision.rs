//! Resolving revisions against a store (ADR-0036 *Revisions*).
//!
//! [`Revision`] checks the grammar; this module decides what a revision
//! names. Revisions that read HEAD, such as `HEAD~1`, need the worktree
//! whose HEAD they read, and fail with [`ResolveError::NoWorktree`]
//! without one.

use alloc::string::String;

use sekai_storage::MetaStore;
use sekai_util::{CommitId, Revision, RevisionBase, RevisionStep, WorktreeName};

use super::graph::commit_parents;
use super::head::born_head;
use super::resolve::{ResolveError, resolve_commit_ref};

/// Resolve `revision` to a commit.
///
/// `worktree` is the worktree whose HEAD the revision may read, or `None`
/// outside every worktree.
pub async fn resolve_revision<M: MetaStore>(
    meta: &M,
    worktree: Option<&WorktreeName>,
    revision: &Revision,
) -> Result<CommitId, ResolveError<M::Error>> {
    let mut commit = resolve_base(meta, worktree, revision.base()).await?;
    for &step in revision.steps() {
        commit =
            take_step(meta, commit, step)
                .await?
                .ok_or_else(|| ResolveError::UnknownRevision {
                    revision: String::from(revision.as_str()),
                })?;
    }
    Ok(commit)
}

async fn resolve_base<M: MetaStore>(
    meta: &M,
    worktree: Option<&WorktreeName>,
    base: &RevisionBase,
) -> Result<CommitId, ResolveError<M::Error>> {
    match base {
        RevisionBase::Head => born_head(meta, worktree).await,
        RevisionBase::Name(name) => resolve_commit_ref(meta, name.as_str()).await,
    }
}

/// The commit `step` leads to from `commit`, or `None` when the parent
/// it names does not exist.
async fn take_step<M: MetaStore>(
    meta: &M,
    commit: CommitId,
    step: RevisionStep,
) -> Result<Option<CommitId>, ResolveError<M::Error>> {
    match step {
        // Git's `get_nth_ancestor`: follow the first parent `n` times.
        RevisionStep::Ancestor(n) => {
            let mut at = commit;
            for _ in 0..n {
                let Some(&first) = commit_parents(meta, at).await?.first() else {
                    return Ok(None);
                };
                at = first;
            }
            Ok(Some(at))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{
        MemMeta, add_ref, block_on, linear_history, main_worktree, ref_name, worktree_name,
    };
    use alloc::string::ToString;
    use sekai_util::Ref;

    fn rev(text: &str) -> Revision {
        Revision::parse(text).unwrap()
    }

    fn resolve(
        meta: &MemMeta,
        text: &str,
    ) -> Result<CommitId, ResolveError<crate::support::MemError>> {
        block_on(resolve_revision(meta, Some(&main_worktree()), &rev(text)))
    }

    #[test]
    fn walks_parents_from_head_and_names() {
        let (meta, [a, b, c]) = linear_history();
        for (text, expected) in [
            ("HEAD", c),
            ("@", c),
            ("main", c),
            ("HEAD~", b),
            ("@~2", a),
            ("HEAD~0", c),
            ("main~1~1", a),
        ] {
            assert_eq!(resolve(&meta, text), Ok(expected), "{text}");
        }
        let c_hex = c.to_string();
        assert_eq!(resolve(&meta, &alloc::format!("{c_hex}~2")), Ok(a));
    }

    #[test]
    fn a_step_past_the_root_is_unknown() {
        let (meta, _) = linear_history();
        for text in ["HEAD~3", "HEAD~2~1", "main~2~1"] {
            assert_eq!(
                resolve(&meta, text),
                Err(ResolveError::UnknownRevision {
                    revision: text.into()
                }),
                "{text}"
            );
        }
    }

    #[test]
    fn an_ambiguous_name_is_an_error() {
        // A tag and a branch named `x` make the spelling ambiguous;
        // the base resolution refuses before any step runs.
        let (mut meta, [a, b, _]) = linear_history();
        add_ref(&mut meta, &Ref::tag(ref_name("x")), a);
        add_ref(&mut meta, &Ref::branch(ref_name("x")), b);
        assert_eq!(
            resolve(&meta, "x~0"),
            Err(ResolveError::AmbiguousRef {
                input: String::from("x"),
                matching: alloc::vec![String::from("refs/tags/x"), String::from("refs/heads/x"),],
            })
        );
    }

    #[test]
    fn head_needs_a_born_head() {
        let mut meta = MemMeta::default();
        meta.with_main();
        assert_eq!(
            resolve(&meta, "HEAD"),
            Err(ResolveError::UnbornHead {
                branch: ref_name("main")
            })
        );
    }

    #[test]
    fn every_head_read_checks_the_worktree() {
        let (meta, _) = linear_history();
        let ghost = worktree_name("ghost");
        for text in ["HEAD", "@", "HEAD~1"] {
            assert_eq!(
                block_on(resolve_revision(&meta, None, &rev(text))),
                Err(ResolveError::NoWorktree),
                "{text}"
            );
            assert_eq!(
                block_on(resolve_revision(&meta, Some(&ghost), &rev(text))),
                Err(ResolveError::UnknownWorktree {
                    name: ghost.clone()
                }),
                "{text}"
            );
        }
    }
}
