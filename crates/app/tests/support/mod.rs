//! A store set up for tests and benches that drive a world by hand.

use std::path::Path;

use sekai_app::{AppError, DEFAULT_REF_NAME, RefName, SekaiInstance};
use sekai_core::{Head, ReflogNote, Worktree};
use sekai_storage::MetaStore as _;
use sekai_storage_local::LocalStore;

/// The store at `root`, created when missing, with a main worktree whose
/// HEAD starts unborn on [`DEFAULT_REF_NAME`].
///
/// Commits act on that worktree, and nothing in the store registers it
/// yet, so tests and benches register it here. Its world root is the
/// directory holding the store, the path `init` would record.
pub async fn init_store(root: &str) -> Result<SekaiInstance<LocalStore>, AppError> {
    let mut instance = SekaiInstance::new(sekai_storage_local::init_local(root).await?);
    let world_root = Path::new(root)
        .parent()
        .unwrap_or_else(|| Path::new(root))
        .to_string_lossy()
        .into_owned();
    if instance
        .store()
        .meta()
        .lookup_worktree(&world_root)
        .await?
        .is_none()
    {
        let main = Worktree {
            path: world_root,
            main: true,
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
