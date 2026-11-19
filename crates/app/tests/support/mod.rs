//! A store set up for tests and benches that drive a world by hand.

use sekai_app::{AppError, DEFAULT_REF_NAME, RefName, SekaiInstance};
use sekai_core::{Head, ReflogNote, Worktree, WorktreeName};
use sekai_storage::MetaStore as _;
use sekai_storage_local::LocalStore;

/// The store at `root`, created when missing, with a main worktree named
/// `main` whose HEAD starts unborn on [`DEFAULT_REF_NAME`].
///
/// Commits act on that worktree, and nothing in the store registers it
/// yet, so tests and benches register it here.
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
            name,
            path: None,
            head: Head::Branch(
                RefName::parse(DEFAULT_REF_NAME).expect("the default branch name is valid"),
            ),
        };
        // An unborn HEAD logs nothing, so the note is never recorded.
        instance
            .store_mut()
            .meta_mut()
            .add_worktree(&main, ReflogNote::new("", 0))
            .await?;
    }
    Ok(instance)
}
