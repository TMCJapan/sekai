//! Worktree registration: a store link that points another world folder
//! at this store.

use std::io::Write as _;
use std::path::Path;

use sekai_storage::{StorageError, io_error};

use crate::LocalStore;

impl LocalStore {
    /// Register a worktree by writing a store link at `path`, the full
    /// store location including the final `.sekai` component. The link
    /// names this store's absolute root, so opening it resolves back here
    /// and the worktree shares this store's history, blobs, and cache.
    ///
    /// The link is created exclusively and synced, like the version file:
    /// an existing file or directory at `path` is refused rather than
    /// overwritten, and a crash cannot leave a torn link. The link format
    /// is UTF-8 text, so a store root that is not valid UTF-8 fails loudly
    /// instead of writing a lossy path.
    pub fn add_worktree(&self, path: impl AsRef<Path>) -> Result<(), StorageError> {
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
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|source| io_error(path.display().to_string(), source))?;
        file.write_all(format!("{root}\n").as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|source| io_error(path.display().to_string(), source))?;
        // TODO: initialize the worktree directory
        Ok(())
    }
}
