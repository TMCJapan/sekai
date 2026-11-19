//! Turning user input into refs and commits (ADR-0035 *Resolution*).
//!
//! Resolution follows gitrevisions: a 64-character hex string is a commit
//! ID, and anything else is tried as `<input>`, `refs/<input>`,
//! `refs/tags/<input>`, and `refs/heads/<input>`, in that order. An
//! ambiguous input is an error **(deviation)**: a backup tool must not
//! guess which of two same-named refs the user meant.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use sekai_storage::MetaStore;
use sekai_util::{CommitId, CommitRef, Ref, RefName, WorktreeName};

use super::graph::CommitError;

/// Failures while resolving a commit reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError<M> {
    /// Metadata backend failure.
    Meta(M),
    /// No commit with this ID exists.
    UnknownCommit {
        /// Requested commit ID.
        id: CommitId,
    },
    /// No ref matches this name.
    UnknownRef {
        /// Requested ref name.
        name: RefName,
    },
    /// Reference is neither a commit ID nor a ref name.
    InvalidRef {
        /// Raw reference text.
        raw: String,
    },
    /// The revision reads HEAD, but no worktree was given.
    NoWorktree,
    /// The worktree whose HEAD the revision reads does not exist.
    UnknownWorktree {
        /// Requested worktree.
        name: WorktreeName,
    },
    /// The revision reads HEAD, which is unborn.
    UnbornHead {
        /// Branch HEAD names.
        branch: RefName,
    },
    /// `reflog show <ref>` names a ref without a reflog: a tag.
    NoReflog {
        /// Ref the name resolved to.
        name: Ref,
    },
    /// A `~` step goes past a commit's parents.
    UnknownRevision {
        /// Revision as written.
        revision: String,
    },
    /// The input names more than one ref.
    AmbiguousRef {
        /// Input as given.
        input: String,
        /// Matching refs, in gitrevisions order.
        matching: Vec<String>,
    },
}

impl<M> From<CommitError<M>> for ResolveError<M> {
    fn from(err: CommitError<M>) -> Self {
        match err {
            CommitError::Meta(err) => Self::Meta(err),
            CommitError::UnknownCommit { id } => Self::UnknownCommit { id },
        }
    }
}

impl<M: fmt::Debug> fmt::Display for ResolveError<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Meta(_) => write!(f, "commit metadata failed"),
            Self::UnknownCommit { id } => write!(f, "unknown commit: {id}"),
            Self::UnknownRef { name } => write!(f, "unknown ref: {name}"),
            Self::InvalidRef { raw } => write!(
                f,
                "invalid reference: {raw:?}, expected <64-char commit hash> or <ref>"
            ),
            Self::NoWorktree => write!(f, "HEAD needs a worktree"),
            Self::UnknownWorktree { name } => write!(f, "unknown worktree: {name}"),
            Self::UnbornHead { branch } => write!(
                f,
                "your current branch '{branch}' does not have any commits yet"
            ),
            Self::NoReflog { name } => write!(f, "{name} has no reflog"),
            Self::UnknownRevision { revision } => write!(f, "unknown revision: {revision}"),
            Self::AmbiguousRef { input, matching } => write!(
                f,
                "refname '{input}' is ambiguous (matched: {})",
                matching.join(", ")
            ),
        }
    }
}

/// The refs gitrevisions tries for `input`, in its order: `<input>`,
/// `refs/<input>`, `refs/tags/<input>`, and `refs/heads/<input>`.
///
/// A rule whose result is not a refname the store can hold yields `None`;
/// only `refs/heads/` and `refs/tags/` exist, so `<input>` matches a full
/// refname and `refs/<input>` a `heads/` or `tags/` form.
fn candidates(input: &str) -> [Option<Ref>; 4] {
    let short = RefName::parse(input).ok();
    [
        Ref::from_full(input).ok(),
        Ref::from_full(&format!("refs/{input}")).ok(),
        short.clone().map(Ref::tag),
        short.map(Ref::branch),
    ]
}

/// Every stored ref `input` may name, in gitrevisions order.
///
/// This is the list Git's `expand_ref` counts: resolution takes the first
/// and warns when there are more. An input that is not a valid ref name
/// matches nothing.
pub(super) async fn matching_refs<M: MetaStore>(
    meta: &M,
    input: &str,
) -> Result<Vec<CommitRef>, ResolveError<M::Error>> {
    let mut found = Vec::new();
    for candidate in candidates(input).into_iter().flatten() {
        if let Some(record) = meta
            .lookup_ref(&candidate)
            .await
            .map_err(ResolveError::Meta)?
        {
            found.push(record);
        }
    }
    Ok(found)
}

/// Resolve a ref name by the gitrevisions rules.
///
/// When several refs match, resolution is an error **(deviation)**; Git
/// warns and takes the first.
pub async fn resolve_ref<M: MetaStore>(
    meta: &M,
    input: &str,
) -> Result<CommitRef, ResolveError<M::Error>> {
    let name = RefName::parse(input).map_err(|_| ResolveError::InvalidRef {
        raw: String::from(input),
    })?;
    let found = matching_refs(meta, input).await?;
    if found.len() > 1 {
        return Err(ResolveError::AmbiguousRef {
            input: String::from(input),
            matching: found.iter().map(|r| r.name.full_name()).collect(),
        });
    }
    found
        .into_iter()
        .next()
        .ok_or(ResolveError::UnknownRef { name })
}

/// Resolve a commit reference: a 64-character hexadecimal commit ID or a
/// ref name.
///
/// As in Git's `get_oid_basic`, a 64-character hex spelling is always a
/// commit ID, never a ref. A ref of the same name makes the spelling
/// ambiguous **(deviation)**; Git warns and resolves the commit.
pub async fn resolve_commit_ref<M: MetaStore>(
    meta: &M,
    raw: &str,
) -> Result<CommitId, ResolveError<M::Error>> {
    let Ok(id) = raw.parse::<CommitId>() else {
        return resolve_ref(meta, raw).await.map(|r| r.commit);
    };
    let shadowed = matching_refs(meta, raw).await?;
    if !shadowed.is_empty() {
        return Err(ResolveError::AmbiguousRef {
            input: String::from(raw),
            matching: shadowed.iter().map(|r| r.name.full_name()).collect(),
        });
    }
    meta.lookup_commit(id)
        .await
        .map_err(ResolveError::Meta)?
        .ok_or(ResolveError::UnknownCommit { id })?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{MemMeta, add_ref, block_on, ref_name};
    use alloc::string::ToString;

    /// Two commits, so each ref can point somewhere distinguishable.
    fn store() -> (MemMeta, CommitId, CommitId) {
        let mut meta = MemMeta::default();
        let first = meta.create_commit(100, &[]).unwrap();
        let second = meta.create_commit(200, &[first]).unwrap();
        (meta, first, second)
    }

    #[test]
    fn resolves_hashes_and_refs() {
        let (mut meta, first, second) = store();
        add_ref(&mut meta, &Ref::branch(ref_name("main")), second);

        let first_hex = first.to_string();
        assert_eq!(block_on(resolve_commit_ref(&meta, &first_hex)), Ok(first));
        assert_eq!(block_on(resolve_commit_ref(&meta, "main")), Ok(second));
        assert_eq!(
            block_on(resolve_commit_ref(&meta, &"f".repeat(64))),
            Err(ResolveError::UnknownCommit {
                id: CommitId::from_hex("f".repeat(64).as_bytes()).unwrap()
            })
        );
        assert_eq!(
            block_on(resolve_commit_ref(&meta, "other")),
            Err(ResolveError::UnknownRef {
                name: ref_name("other")
            })
        );
        for raw in ["", " 1", "a b", "@123", "@main", "HEAD"] {
            assert_eq!(
                block_on(resolve_commit_ref(&meta, raw)),
                Err(ResolveError::InvalidRef { raw: raw.into() }),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn tags_and_qualified_forms_resolve() {
        let (mut meta, first, second) = store();
        add_ref(&mut meta, &Ref::tag(ref_name("v1")), first);
        add_ref(&mut meta, &Ref::branch(ref_name("dev")), second);

        for (input, commit) in [
            ("v1", first),
            ("tags/v1", first),
            ("refs/tags/v1", first),
            ("dev", second),
            ("heads/dev", second),
            ("refs/heads/dev", second),
        ] {
            assert_eq!(
                block_on(resolve_commit_ref(&meta, input)),
                Ok(commit),
                "{input}"
            );
        }
        assert_eq!(
            block_on(resolve_ref(&meta, "heads/dev")).unwrap().name,
            Ref::branch(ref_name("dev"))
        );
    }

    #[test]
    fn more_than_one_match_is_an_error() {
        // `x` is both a tag and a branch: `refs/tags/x` comes first in
        // gitrevisions order, but the spelling is ambiguous and sekai
        // refuses rather than guesses (deviation).
        let (mut meta, first, second) = store();
        add_ref(&mut meta, &Ref::tag(ref_name("x")), first);
        add_ref(&mut meta, &Ref::branch(ref_name("x")), second);
        assert_eq!(
            block_on(resolve_commit_ref(&meta, "x")),
            Err(ResolveError::AmbiguousRef {
                input: String::from("x"),
                matching: alloc::vec![String::from("refs/tags/x"), String::from("refs/heads/x"),],
            })
        );

        // A full refname also names a branch spelled the same way; the
        // exact key is among the matches, so the spelling is ambiguous.
        add_ref(&mut meta, &Ref::branch(ref_name("refs/heads/x")), first);
        assert_eq!(
            block_on(resolve_commit_ref(&meta, "refs/heads/x")),
            Err(ResolveError::AmbiguousRef {
                input: String::from("refs/heads/x"),
                matching: alloc::vec![
                    String::from("refs/heads/x"),
                    String::from("refs/heads/refs/heads/x"),
                ],
            })
        );
    }

    #[test]
    fn a_hex_name_that_matches_a_ref_is_ambiguous() {
        // A 64-character hex spelling is a commit ID, and a ref of the
        // same name makes the spelling ambiguous (deviation).
        let (mut meta, first, second) = store();
        let hex = "a".repeat(64);
        add_ref(&mut meta, &Ref::branch(ref_name(&hex)), first);
        assert_eq!(
            block_on(resolve_commit_ref(&meta, &hex)),
            Err(ResolveError::AmbiguousRef {
                input: hex.clone(),
                matching: alloc::vec![alloc::format!("refs/heads/{hex}")],
            })
        );

        // A known one is ambiguous too, not the commit ID.
        let second_hex = second.to_string();
        add_ref(&mut meta, &Ref::tag(ref_name(&second_hex)), first);
        assert_eq!(
            block_on(resolve_commit_ref(&meta, &second_hex)),
            Err(ResolveError::AmbiguousRef {
                input: second_hex.clone(),
                matching: alloc::vec![alloc::format!("refs/tags/{second_hex}")],
            })
        );
    }

    #[test]
    fn an_unborn_branch_resolves_to_nothing() {
        // HEAD names `main`, but the branch has no commit and no row yet.
        let mut meta = MemMeta::default();
        meta.with_main();
        assert_eq!(
            block_on(resolve_commit_ref(&meta, "main")),
            Err(ResolveError::UnknownRef {
                name: ref_name("main")
            })
        );
    }
}
