//! Branches and the HEAD new commits land on (ADR-0035).
//!
//! Commands act on the instance's worktree
//! ([`SekaiInstance::worktree`]): a commit moves the branch its HEAD is
//! on, and `checkout` switches HEAD. The store refuses to delete or
//! force-move a branch that a HEAD has checked out, inside the
//! transaction that would do it.

use sekai_core::{
    CommitId, CommitRef, HeadState, Ref, RefKind, RefName, Reflog, ReflogEntry, ReflogNote,
    ReflogSelector, ResolveError, Revision, WorktreeName, branch_holders,
};
use sekai_storage::{MetaStore, SekaiStore, StorageError};

use crate::error::{AppError, BranchUse, Lookup, NameKind};
use crate::instance::SekaiInstance;
use crate::time::now_ms;

/// Translate the store's branch errors into the app's, naming refs as
/// Git's porcelain does: `main`, not `refs/heads/main`. A branch the
/// store finds checked out is refused for `action`.
///
/// The app checks for a checked-out branch before it writes, naming the
/// worktree by its root; the store's own refusal, which only a
/// concurrent checkout reaches, names the worktree.
pub(crate) const fn ref_err(action: BranchUse) -> impl Fn(StorageError) -> AppError {
    move |err| match err {
        StorageError::UnknownCommit { id } => {
            AppError::CommitRef(sekai_core::ResolveError::UnknownCommit { id })
        }
        StorageError::BranchCheckedOut { branch, worktree } => AppError::BranchCheckedOut {
            action,
            branch: branch.name.to_string(),
            worktree,
        },
        StorageError::UnknownRef { name } => AppError::NotFound {
            kind: name.kind.into(),
            name: name.name.to_string(),
        },
        StorageError::RefExists { name } => AppError::AlreadyExists {
            kind: name.kind.into(),
            name: name.name.to_string(),
        },
        other => AppError::Storage(other),
    }
}

/// Translate a failed [`sekai_core::switch_head`]: the store's errors as
/// [`ref_err`] names them, and HEAD's own as [`AppError::CommitRef`].
pub(crate) fn switch_err(err: ResolveError<StorageError>) -> AppError {
    match err {
        ResolveError::Meta(err) => ref_err(BranchUse::CheckOut)(err),
        other => AppError::CommitRef(other),
    }
}

/// Where a ref a command created or moved points now, and where it
/// pointed before a move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefUpdate {
    /// The commit the ref names now.
    pub commit: CommitId,
    /// Where a moved ref pointed before; `None` for a created one.
    pub previous: Option<CommitId>,
}

/// What `branch -m` renamed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Renamed {
    /// The branch's name before the rename.
    pub old: RefName,
    /// The commit the branch names, or `None` for an unborn branch.
    pub commit: Option<CommitId>,
}

/// A branch in a listing, with the worktree that has it checked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchEntry {
    /// The branch and the commit it names.
    pub branch: CommitRef,
    /// The worktree whose HEAD is on the branch, if any.
    pub worktree: Option<WorktreeName>,
    /// Whether that worktree is the instance's own, which `branch` marks
    /// with `*`; another worktree's gets `+`.
    pub head: bool,
}

/// Create `name` at `commit`, or move it to `commit` when `force` and it
/// already exists; `created`/`reset` are the reflog notes for the two
/// cases. Returns the commit it pointed at before, if any.
async fn upsert_ref<M: MetaStore<Error = StorageError>>(
    meta: &mut M,
    name: &Ref,
    commit: CommitId,
    force: bool,
    created: &str,
    reset: &str,
    at_ms: u64,
) -> Result<Option<CommitId>, AppError> {
    match meta
        .create_ref(name, commit, ReflogNote::new(created, at_ms))
        .await
    {
        Ok(()) => Ok(None),
        Err(StorageError::RefExists { .. }) if force => Ok(Some(
            meta.move_ref(name, commit, ReflogNote::new(reset, at_ms))
                .await
                .map_err(ref_err(BranchUse::ForceUpdate))?,
        )),
        Err(err) => Err(ref_err(BranchUse::ForceUpdate)(err)),
    }
}

impl<S: SekaiStore> SekaiInstance<S> {
    /// Fail when a worktree other than `allowed` has `branch` checked
    /// out, born or unborn, refusing it for `action`.
    pub(crate) async fn refuse_checked_out(
        &self,
        branch: &RefName,
        allowed: Option<&WorktreeName>,
        action: BranchUse,
    ) -> Result<(), AppError> {
        let Some(holder) =
            sekai_core::checked_out_holder(self.store.meta(), branch, allowed).await?
        else {
            return Ok(());
        };
        let worktree = match (holder.path, &self.main_root) {
            (Some(path), _) => path,
            (None, Some(root)) => root.display().to_string(),
            (None, None) => holder.name.to_string(),
        };
        Err(AppError::BranchCheckedOut {
            action,
            branch: branch.to_string(),
            worktree,
        })
    }

    /// List refs in full refname order.
    pub async fn list_refs(&self) -> Result<Vec<CommitRef>, AppError> {
        Ok(sekai_core::usecase::refs::list_refs(self.store.meta()).await?)
    }

    /// Point branch `name` at `start`, as `branch [-f] <name> [<start>]`
    /// does; `start` is [`Revision::head`] when none is given.
    ///
    /// An existing branch fails with [`AppError::AlreadyExists`] unless
    /// `force` moves it, and a checked-out branch is never moved. As in
    /// Git, both are refused before `start` resolves. The reflog names
    /// `start` as written, as Git's does.
    pub async fn create_branch(
        &mut self,
        name: &RefName,
        start: &Revision,
        force: bool,
    ) -> Result<RefUpdate, AppError> {
        let branch = Ref::branch(name.clone());
        let meta = self.store.meta();
        if meta.lookup_ref(&branch).await?.is_some() {
            if !force {
                return Err(AppError::AlreadyExists {
                    kind: NameKind::Branch,
                    name: name.to_string(),
                });
            }
            self.refuse_checked_out(name, None, BranchUse::ForceUpdate)
                .await?;
        }
        self.write_ref(&branch, start, force).await
    }

    /// Point tag `name` at `target`, as `tag [-f] <name> [<commit-ish>]`
    /// does.
    ///
    /// An existing tag fails with [`AppError::AlreadyExists`] unless `force`
    /// replaces it. Unlike a branch, `target` resolves first, as in Git.
    pub async fn create_tag(
        &mut self,
        name: &RefName,
        target: &Revision,
        force: bool,
    ) -> Result<RefUpdate, AppError> {
        self.write_ref(&Ref::tag(name.clone()), target, force).await
    }

    async fn write_ref(
        &mut self,
        name: &Ref,
        revision: &Revision,
        force: bool,
    ) -> Result<RefUpdate, AppError> {
        let commit = self.resolve_revision(revision).await?;
        let at_ms = now_ms()?;
        // Tags keep no reflog, so the store records these for branches only.
        let created = format!("branch: Created from {revision}");
        let reset = format!("branch: Reset to {revision}");
        let meta = self.store.meta_mut();
        let previous = upsert_ref(meta, name, commit, force, &created, &reset, at_ms).await?;
        Ok(RefUpdate { commit, previous })
    }

    /// Delete branch `name`, returning the commit it named.
    ///
    /// Without `force`, a branch HEAD does not reach fails with
    /// [`AppError::NotFullyMerged`], as `branch -d` does; `force` deletes
    /// it anyway, as `branch -D` does. A checked-out branch is never
    /// deleted, unborn too, and Git refuses it before anything else.
    pub async fn delete_branch(
        &mut self,
        name: &RefName,
        force: bool,
    ) -> Result<CommitId, AppError> {
        let branch = Ref::branch(name.clone());
        self.refuse_checked_out(name, None, BranchUse::Delete)
            .await?;
        let meta = self.store.meta();
        let Some(record) = meta.lookup_ref(&branch).await? else {
            return Err(AppError::NotFound {
                kind: Lookup::Branch,
                name: name.to_string(),
            });
        };
        if !force && !sekai_core::head_reaches(meta, self.worktree.as_ref(), record.commit).await? {
            return Err(AppError::NotFullyMerged {
                branch: name.to_string(),
            });
        }
        self.remove_ref(&branch).await
    }

    /// Delete tag `name`, returning the commit it named.
    pub async fn delete_tag(&mut self, name: &RefName) -> Result<CommitId, AppError> {
        self.remove_ref(&Ref::tag(name.clone())).await
    }

    async fn remove_ref(&mut self, name: &Ref) -> Result<CommitId, AppError> {
        self.store
            .meta_mut()
            .delete_ref(name)
            .await
            .map_err(ref_err(BranchUse::Delete))?
            .ok_or_else(|| AppError::NotFound {
                kind: name.kind.into(),
                name: name.name.to_string(),
            })
    }

    /// Rename branch `old` to `new`, over an existing `new` when `force`
    /// is set, as `branch (-m | -M) [<old>] <new>` does. Returns the
    /// branch renamed and the commit it names, `None` for an unborn one.
    ///
    /// Without `old`, HEAD's branch is renamed, born or unborn; a
    /// detached HEAD fails with [`AppError::RenameDetachedHead`].
    pub async fn rename_branch(
        &mut self,
        old: Option<&RefName>,
        new: &RefName,
        force: bool,
    ) -> Result<Renamed, AppError> {
        let old = match old {
            Some(old) => old.clone(),
            None => sekai_core::head_branch(self.store.meta(), self.worktree.as_ref())
                .await?
                .ok_or(AppError::RenameDetachedHead)?,
        };
        if force && *new != old {
            self.refuse_checked_out(new, None, BranchUse::ForceUpdate)
                .await?;
        }
        let heads = RefKind::Branch.prefix();
        let message = format!("Branch: renamed {heads}{old} to {heads}{new}");
        self.store
            .meta_mut()
            .rename_branch(&old, new, force, ReflogNote::new(&message, now_ms()?))
            .await
            .map_err(|err| match err {
                StorageError::UnknownRef { name } => AppError::NotFound {
                    kind: Lookup::RenamedBranch,
                    name: name.name.to_string(),
                },
                other => ref_err(BranchUse::ForceUpdate)(other),
            })?;
        let commit = self
            .store
            .meta()
            .lookup_ref(&Ref::branch(new.clone()))
            .await?
            .map(|renamed| renamed.commit);
        Ok(Renamed { old, commit })
    }

    /// Every branch in refname order whose name matches one of
    /// `patterns`, or every branch without patterns, as `branch --list`
    /// lists them, with the worktree that has each checked out. Patterns
    /// are globs as [`sekai_core::wildmatch`] matches them.
    pub async fn list_branches(&self, patterns: &[String]) -> Result<Vec<BranchEntry>, AppError> {
        let mut holders = branch_holders(self.store.meta()).await?;
        Ok(self
            .list_refs_matching(RefKind::Branch, patterns)
            .await?
            .into_iter()
            .map(|branch| {
                let worktree = holders.remove(&branch.name.name);
                BranchEntry {
                    head: worktree.is_some() && worktree.as_ref() == self.worktree.as_ref(),
                    worktree,
                    branch,
                }
            })
            .collect())
    }

    /// Every tag in refname order whose name matches one of `patterns`,
    /// or every tag without patterns, as `tag -l` lists them.
    pub async fn list_tags(&self, patterns: &[String]) -> Result<Vec<CommitRef>, AppError> {
        self.list_refs_matching(RefKind::Tag, patterns).await
    }

    async fn list_refs_matching(
        &self,
        kind: RefKind,
        patterns: &[String],
    ) -> Result<Vec<CommitRef>, AppError> {
        let matches = |name: &RefName| {
            patterns.is_empty()
                || patterns
                    .iter()
                    .any(|pattern| sekai_core::wildmatch(pattern, name.as_str()))
        };
        Ok(self
            .list_refs()
            .await?
            .into_iter()
            .filter(|r| r.name.kind == kind && matches(&r.name.name))
            .collect())
    }

    /// The reflog `selector` names, checked against the store, as
    /// `reflog show [<ref>]` reads it.
    pub async fn resolve_reflog(&self, selector: &ReflogSelector) -> Result<Reflog, AppError> {
        sekai_core::resolve_reflog(self.store.meta(), self.worktree.as_ref(), selector)
            .await
            .map_err(AppError::CommitRef)
    }

    /// Up to `limit` of `log`'s entries, newest first.
    pub async fn read_reflog(
        &self,
        log: &Reflog,
        limit: usize,
    ) -> Result<Vec<ReflogEntry>, AppError> {
        sekai_core::read_reflog(self.store.meta(), log, limit)
            .await
            .map_err(AppError::CommitRef)
    }

    /// HEAD resolved against the refs, or `None` outside every worktree
    /// or when the worktree does not exist.
    pub async fn head(&self) -> Result<Option<HeadState>, AppError> {
        let Some(worktree) = &self.worktree else {
            return Ok(None);
        };
        Ok(sekai_core::head(self.store.meta(), worktree).await?)
    }

    /// The branch HEAD is on, born or unborn, or `None` when HEAD is
    /// detached, outside every worktree, or when the worktree does not
    /// exist.
    pub async fn head_branch(&self) -> Result<Option<RefName>, AppError> {
        Ok(self.head().await?.and_then(|head| head.branch().cloned()))
    }

    /// The commit HEAD stands at - the one the next commit continues -
    /// or `None` while HEAD is unborn.
    ///
    /// This is not the newest commit in the store: with several branches,
    /// the newest commit may sit on a branch HEAD is not on.
    pub async fn current_commit_id(&self) -> Result<Option<CommitId>, AppError> {
        Ok(self.head().await?.as_ref().and_then(HeadState::commit))
    }
}
