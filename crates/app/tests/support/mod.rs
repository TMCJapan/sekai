//! A store set up for tests and benches that drive a world by hand.

use std::path::Path;

use sekai_app::{AppError, SekaiInstance};
use sekai_storage_local::LocalStore;

/// The store at `root`, created when missing, with the directory holding
/// it registered as the main worktree.
///
/// Tests and benches that only need a store to commit into create it here
/// instead of through `SekaiInstance::init`, so the setup a store needs
/// before commits can land on it lives in one place.
pub async fn init_store(root: impl AsRef<Path>) -> Result<SekaiInstance<LocalStore>, AppError> {
    let root = root.as_ref();
    SekaiInstance::init(root, root.parent().unwrap_or(root)).await
}
