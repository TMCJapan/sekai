//! A store set up for tests and benches that drive a world by hand.

use sekai_app::{AppError, DEFAULT_REF_NAME, Head, RefName, SekaiInstance, WorktreeName};
use sekai_core::{Commit, ReflogNote, Worktree};
use sekai_storage::MetaStore as _;
use sekai_storage_local::LocalStore;

/// The store at `root`, created when missing, acting on a main worktree
/// named `main` whose HEAD starts unborn on [`DEFAULT_REF_NAME`].
///
/// `init` registers a world it can recognize (ADR-0035); tests and
/// benches that build their world afterwards, or serve it from a fake
/// tree, register the main worktree here instead.
pub async fn init_store(root: &str) -> Result<SekaiInstance<LocalStore>, AppError> {
    let mut instance = SekaiInstance::new(sekai_storage_local::init_local(root).await?);
    let name = WorktreeName::parse("main")?;
    if instance
        .store()
        .meta()
        .lookup_worktree(&name)
        .await?
        .is_none()
    {
        let main = Worktree {
            name: name.clone(),
            path: None,
            head: Head::Branch(RefName::parse(DEFAULT_REF_NAME)?),
        };
        // An unborn HEAD logs nothing, so the note is never recorded.
        instance
            .store_mut()
            .meta_mut()
            .add_worktree(&main, ReflogNote::new("", 0))
            .await?;
    }
    Ok(instance.with_worktree(Some(name)))
}

/// Every commit in `instance`'s store, oldest first, as the Git walk
/// reports them. Single-branch, linear test histories only. Only some
/// test crates call it.
#[allow(dead_code)]
pub async fn all_commits(instance: &SekaiInstance<LocalStore>) -> Vec<Commit> {
    let meta = instance.store().meta();
    let bounds = sekai_core::resolve_log_bounds(meta, None, &[], true)
        .await
        .unwrap();
    let mut commits = sekai_core::walk_log(meta, &bounds).await.unwrap();
    commits.reverse();
    commits
}
