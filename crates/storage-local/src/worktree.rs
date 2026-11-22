//! Worktrees: the worlds a store backs up, each keyed by the store
//! location it opens. The store's own (main) worktree is registered at
//! init; a linked worktree is a store link that points another world
//! folder at this store.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use sekai_storage::{StorageError, io_error};
use sekai_util::Head;

use crate::LocalStore;

/// One worktree registered in a store, keyed by the absolute store
/// location it opens: the store root itself for the store's own (main)
/// worktree, or a store link file for a linked one.
///
/// Ordering and uniqueness are byte-wise and case-sensitive, over the
/// path alone: no separate name or ID exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// Absolute store location: the store root, or a store link file.
    pub path: PathBuf,
    /// What HEAD stores.
    pub head: Head,
}

impl LocalStore {
    /// Register a linked worktree by writing a store link at `path`, the
    /// full store location including the final `.sekai` component. The
    /// link names this store's absolute root, so opening it resolves back
    /// here and the worktree shares this store's history, blobs, and
    /// cache. The worktree row is keyed by `path` itself, made absolute,
    /// with `head` as its HEAD.
    ///
    /// The link is created exclusively and synced, like the version file:
    /// an existing file or directory at `path` is refused rather than
    /// overwritten, and a crash cannot leave a torn link. The link format
    /// is UTF-8 text, so a store root that is not valid UTF-8 fails loudly
    /// instead of writing a lossy path.
    pub async fn add_worktree(
        &self,
        path: impl AsRef<Path>,
        head: Head,
    ) -> Result<(), StorageError> {
        let path = path.as_ref();
        let root = self.root.to_str().ok_or_else(|| {
            StorageError::backend(
                "store link",
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "store root is not valid UTF-8",
                ),
            )
        })?;
        let key = store_key(path)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|source| io_error(path.display().to_string(), source))?;
        file.write_all(format!("{root}\n").as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|source| io_error(path.display().to_string(), source))?;

        self.meta.add_worktree(&key, head).await?;
        Ok(())
    }

    /// The worktrees registered in this store, in byte-wise path order.
    pub async fn worktrees(&self) -> Result<Vec<Worktree>, StorageError> {
        self.meta.worktrees().await
    }

    /// Register the store itself as its main worktree, keyed by the
    /// canonical store root with HEAD unborn on the default branch.
    /// Called by init; a re-init keeps the existing row and its HEAD.
    pub(crate) async fn init_worktree(&self) -> Result<(), StorageError> {
        let key = store_key(&self.root)?;
        self.meta.init_worktree(&key).await
    }
}

/// The absolute UTF-8 spelling of a store location, the key a worktree
/// row carries. A location that is not valid UTF-8 fails loudly instead
/// of storing a lossy path.
fn store_key(path: impl AsRef<Path>) -> Result<String, StorageError> {
    let path = path.as_ref();
    let path =
        std::path::absolute(path).map_err(|source| io_error(path.display().to_string(), source))?;
    path.to_str().map(str::to_owned).ok_or_else(|| {
        StorageError::backend(
            "worktree",
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "worktree path is not valid UTF-8",
            ),
        )
    })
}
