//! The history `log` walks and the names it decorates commits with
//! (ADR-0036 *log*).

use std::collections::BTreeMap;

use sekai_core::{Commit, CommitId, HeadState, Ref, RefKind, RefName, RevisionRange};
use sekai_storage::SekaiStore;

use crate::error::AppError;
use crate::instance::SekaiInstance;

/// Where `log` starts and in which order it lists.
#[derive(Debug, Clone, Copy, Default)]
pub struct LogOptions {
    /// Start from every ref and every detached HEAD, as `--all` does.
    pub all: bool,
    /// List oldest first, as `--reverse` does.
    pub reverse: bool,
    /// Attach each commit's decorations. The terminal with decorations
    /// on and JSON output read them; other output ignores them.
    pub decorate: bool,
}

/// A name `log` decorates a commit with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decoration {
    /// The instance's HEAD, with the branch it is attached to, which Git
    /// prints as `HEAD -> <branch>`.
    Head(Option<RefName>),
    /// A branch or a tag.
    Ref(Ref),
}

/// One `log` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    /// The commit.
    pub commit: Commit,
    /// The names that point at it, in the order Git's `--decorate=short`
    /// prints them.
    pub decorations: Vec<Decoration>,
}

impl<S: SekaiStore> SekaiInstance<S> {
    /// The commits `ranges` and `options` select, newest first in Git's
    /// default order, as `log [--all] [--reverse] [<revision-range>...]`
    /// lists them, each with its decorations.
    ///
    /// Without ranges or `--all`, the walk starts at HEAD, and an unborn
    /// HEAD fails as Git's `log` does.
    pub async fn log(
        &self,
        ranges: &[RevisionRange],
        options: LogOptions,
    ) -> Result<Vec<LogEntry>, AppError> {
        let meta = self.store.meta();
        let bounds =
            sekai_core::resolve_log_bounds(meta, self.worktree.as_ref(), ranges, options.all)
                .await?;
        let mut commits = sekai_core::walk_log(meta, &bounds).await?;
        if options.reverse {
            commits.reverse();
        }
        let mut decorations = if options.decorate {
            self.decorations().await?
        } else {
            BTreeMap::new()
        };
        Ok(commits
            .into_iter()
            .map(|commit| LogEntry {
                decorations: decorations.remove(&commit.id).unwrap_or_default(),
                commit,
            })
            .collect())
    }

    /// The decorations of every decorated commit, in the order Git's
    /// `format_decorations` prints them (git 2.53): Git prepends each
    /// name as it reads the refs in refname order and then HEAD, so HEAD
    /// comes first and the refs follow in reverse refname order. The
    /// branch HEAD is attached to joins HEAD as `HEAD -> <branch>`.
    async fn decorations(&self) -> Result<BTreeMap<CommitId, Vec<Decoration>>, AppError> {
        let head = self.head().await?;
        // An unborn branch has no ref to join HEAD.
        let attached = head.as_ref().and_then(HeadState::branch);
        let mut decorations: BTreeMap<CommitId, Vec<Decoration>> = BTreeMap::new();
        for r in self.list_refs().await?.into_iter().rev() {
            let joins_head = r.name.kind == RefKind::Branch && Some(&r.name.name) == attached;
            if !joins_head {
                decorations
                    .entry(r.commit)
                    .or_default()
                    .push(Decoration::Ref(r.name));
            }
        }
        if let Some(commit) = head.as_ref().and_then(HeadState::commit) {
            decorations
                .entry(commit)
                .or_default()
                .insert(0, Decoration::Head(attached.cloned()));
        }
        Ok(decorations)
    }
}
