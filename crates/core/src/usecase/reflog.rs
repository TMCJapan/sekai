//! Reading reflogs (ADR-0035 *Reflogs*).
//!
//! The store keeps one log per worktree HEAD and one per branch; tags
//! keep none. Entries come newest first.

use alloc::string::String;
use alloc::vec::Vec;

use sekai_storage::MetaStore;
use sekai_util::{Ref, RefKind, RefName, ReflogEntry, ReflogSelector, WorktreeName};

use super::head::head_state;
use super::resolve::{ResolveError, matching_refs};

/// A reflog checked against the store: a HEAD log of a worktree that
/// exists, or the log of a stored branch or of the branch HEAD names.
///
/// Only [`resolve_reflog`] makes one. The store reads an unknown worktree's
/// or branch's log as empty, so an unchecked name would pass for a log
/// with no entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reflog {
    /// How messages name the log, as Git's do: `HEAD`, the current
    /// branch, or the name as written, such as `heads/x`.
    label: String,
    log: Log,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Log {
    Head(WorktreeName),
    Branch(RefName),
}

impl Reflog {
    /// How messages name the log: `HEAD`, the current branch, or the
    /// name as written, with `@{<n>}` appended.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The `HEAD@{<n>}` or `<branch>@{<n>}` form of an entry, as Git
    /// prints it in reflog lines.
    pub fn selector(&self, n: usize) -> String {
        alloc::format!("{}@{{{n}}}", self.label)
    }

    fn head(worktree: &WorktreeName) -> Self {
        Self {
            label: String::from("HEAD"),
            log: Log::Head(worktree.clone()),
        }
    }

    fn branch(branch: RefName, label: &str) -> Self {
        Self {
            label: String::from(label),
            log: Log::Branch(branch),
        }
    }
}

/// The reflog `selector` names, as `reflog show <ref>` reads it
/// (ADR-0036).
///
/// A detached HEAD logs under HEAD, not under the branch it left, so
/// the selector for a detached HEAD reads HEAD's log.
pub async fn resolve_reflog<M: MetaStore>(
    meta: &M,
    worktree: Option<&WorktreeName>,
    selector: &ReflogSelector,
) -> Result<Reflog, ResolveError<M::Error>> {
    match selector {
        ReflogSelector::Head => {
            let (worktree, _) = head_state(meta, worktree).await?;
            Ok(Reflog::head(worktree))
        }
        ReflogSelector::Ref(name) => {
            let branch = logged_branch(meta, name).await?;
            Ok(Reflog::branch(branch, name.as_str()))
        }
    }
}

/// Up to `limit` of `log`'s entries, newest first.
pub async fn read_reflog<M: MetaStore>(
    meta: &M,
    log: &Reflog,
    limit: usize,
) -> Result<Vec<ReflogEntry>, ResolveError<M::Error>> {
    let mut entries = Vec::new();
    let collect = |entry: &ReflogEntry| {
        if entries.len() < limit {
            entries.push(entry.clone());
        }
        entries.len() < limit
    };
    match &log.log {
        Log::Head(worktree) => meta.visit_head_log(worktree, collect).await,
        Log::Branch(branch) => meta.visit_ref_log(branch, collect).await,
    }
    .map_err(ResolveError::Meta)?;
    Ok(entries)
}

/// The branch whose log `reflog show <name>` reads: the branch
/// `<name>` resolves to. Only branches keep a log (ADR-0035).
async fn logged_branch<M: MetaStore>(
    meta: &M,
    name: &RefName,
) -> Result<RefName, ResolveError<M::Error>> {
    let found = matching_refs(meta, name.as_str()).await?;
    let branches: Vec<&Ref> = found
        .iter()
        .map(|record| &record.name)
        .filter(|r| r.kind == RefKind::Branch)
        .collect();
    match (branches.first(), found.first()) {
        (Some(branch), _) => {
            if branches.len() > 1 {
                return Err(ResolveError::AmbiguousRef {
                    input: String::from(name.as_str()),
                    matching: branches.iter().map(|r| r.full_name()).collect(),
                });
            }
            Ok(branch.name.clone())
        }
        (None, Some(tag)) => Err(ResolveError::NoReflog {
            name: tag.name.clone(),
        }),
        (None, None) => Err(ResolveError::UnknownRef { name: name.clone() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{add_ref, block_on, linear_history, main_worktree, ref_name};
    use sekai_util::CommitId;

    #[test]
    fn reading_stops_at_the_limit() {
        let (meta, [_, b, c]) = linear_history();
        let selector = ReflogSelector::Head;
        let log = block_on(resolve_reflog(&meta, Some(&main_worktree()), &selector)).unwrap();
        let read = |limit| -> Vec<CommitId> {
            let entries = block_on(read_reflog(&meta, &log, limit)).unwrap();
            entries.iter().map(|entry| entry.new).collect()
        };
        assert_eq!(read(0), []);
        assert_eq!(read(2), [c, b]);
        assert_eq!(read(usize::MAX).len(), 3);
    }

    #[test]
    fn a_name_that_is_both_a_tag_and_a_branch_reads_the_branches_log() {
        // A tag and a branch with the same name: the branch's log is
        // the one read, like Git's reflog selection.
        let (mut meta, [a, b, _]) = linear_history();
        add_ref(&mut meta, &Ref::tag(ref_name("x")), a);
        add_ref(&mut meta, &Ref::branch(ref_name("x")), b);
        let selector = ReflogSelector::Ref(ref_name("x"));
        let log = block_on(resolve_reflog(&meta, None, &selector)).unwrap();
        match &log.log {
            Log::Branch(branch) => assert_eq!(branch.as_str(), "x"),
            Log::Head(_) => panic!("expected the branch's log"),
        }
    }
}
