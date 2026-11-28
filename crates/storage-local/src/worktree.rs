//! Worktrees: the worlds a store backs up.
//!
//! The store's own (main) worktree is registered at init and is whatever the
//! store root is opened as; a linked worktree is a store link that points
//! another world folder at this store, keyed by the link's location.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use sekai_storage::{StorageError, io_error};
use sekai_util::worktree::Head;

use crate::LocalStore;

/// One worktree registered in a store.
///
/// The main worktree carries no stored path: it is the store root, so
/// it follows the store when the store moves or is copied, and any
/// spelling of the root opens it. A linked worktree is keyed by its
/// store link's location (`store_key`); those keys are unique,
/// byte-wise and case-sensitive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// Absolute store location: the store root as this handle opened it
    /// for the main worktree, the store link file for a linked one.
    pub path: PathBuf,
    /// Whether this is the store's own (main) worktree.
    pub main: bool,
    /// What HEAD stores.
    pub head: Head,
}

impl LocalStore {
    /// Register a linked worktree by writing a store link at `path`, the
    /// full store location including the final `.sekai` component. The
    /// link names this store's absolute root, so opening it resolves back
    /// here and the worktree shares this store's history, blobs, and
    /// cache. The worktree row is keyed by `path` through `store_key`,
    /// with `head` as its HEAD.
    ///
    /// The row is inserted before the link is written: the insert is the
    /// likelier failure (a duplicate path, an already-checked-out branch,
    /// a missing commit for a detached HEAD), and a refused insert must
    /// not leave a link behind that blocks every retry. A failed link
    /// write rolls the fresh row back, so a retry can complete the
    /// registration.
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
        self.meta.add_worktree(&key, head).await?;
        if let Err(source) = write_link(path, root) {
            // The insert just succeeded, so the row is this call's own:
            // drop it to restore the pre-call state.
            self.meta.remove_worktree(&key).await?;
            return Err(source);
        }
        Ok(())
    }

    /// Remove a linked worktree: delete its store link and its row, the
    /// inverse of [`Self::add_worktree`]. Returns the location the removed
    /// worktree was keyed by.
    ///
    /// The world directory the worktree backs up is left untouched: add
    /// never materialized it, so remove never deletes it. Only the link
    /// this store wrote and the registration row are this operation's own.
    ///
    /// The store's own (main) worktree (named by its root) and an
    /// unregistered location are refused, as Git refuses them. A link
    /// that is already gone is not an error: the row is still removed,
    /// like Git cleaning up a worktree whose directory vanished.
    ///
    /// The link is deleted before the row so a failed delete leaves a row
    /// a retry can finish, never a leftover link that no later remove can
    /// reach (an absent row is refused as unregistered).
    pub async fn remove_worktree(&self, path: impl AsRef<Path>) -> Result<PathBuf, StorageError> {
        let path = path.as_ref();
        let key = store_key(path)?;
        if key == store_key(&self.root)? {
            return Err(worktree_refusal(format!(
                "'{}' is a main worktree",
                path.display()
            )));
        }
        let registered = self.meta.worktrees().await?;
        if !registered
            .iter()
            .any(|row| row.link.as_deref() == Some(key.as_str()))
        {
            return Err(worktree_refusal(format!(
                "'{}' is not a worktree",
                path.display()
            )));
        }
        let link = Path::new(&key);
        match std::fs::remove_file(link) {
            Ok(()) => {}
            // A vanished link is Git's missing-worktree case: clean the row.
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error(link.display().to_string(), source)),
        }
        self.meta.remove_worktree(&key).await?;
        Ok(PathBuf::from(key))
    }

    /// The worktrees registered in this store: the main one first, then
    /// the linked ones in byte-wise path order, as Git lists them.
    pub async fn worktrees(&self) -> Result<Vec<Worktree>, StorageError> {
        let rows = self.meta.worktrees().await?;
        Ok(rows
            .into_iter()
            .map(|row| match row.link {
                Some(link) => Worktree {
                    path: PathBuf::from(link),
                    main: false,
                    head: row.head,
                },
                None => Worktree {
                    path: self.root.clone(),
                    main: true,
                    head: row.head,
                },
            })
            .collect())
    }

    /// Register the store itself as its main worktree, with HEAD unborn on
    /// the default branch. Called by init; a re-init keeps the existing
    /// row and its HEAD.
    pub(crate) async fn init_worktree(&self) -> Result<(), StorageError> {
        self.meta.init_worktree().await
    }
}

/// Create the store link at `path` naming `root`, exclusively and synced.
/// A write or sync failure removes the file this call created, so a torn
/// link never survives to block a retry; a file that already existed is
/// left untouched.
fn write_link(path: impl AsRef<Path>, root: &str) -> Result<(), StorageError> {
    let path = path.as_ref();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| io_error(path.display().to_string(), source))?;
    if let Err(source) = file
        .write_all(format!("{root}\n").as_bytes())
        .and_then(|()| file.sync_all())
    {
        let _ = std::fs::remove_file(path);
        return Err(io_error(path.display().to_string(), source));
    }
    Ok(())
}

/// A worktree removal refusal, worded as Git words it. The location is an
/// invalid argument (the main worktree or an unregistered one), not an I/O
/// failure, so it carries `InvalidInput`.
fn worktree_refusal(message: String) -> StorageError {
    StorageError::backend(
        "worktree",
        std::io::Error::new(std::io::ErrorKind::InvalidInput, message),
    )
}

/// The key a linked worktree row carries for the store link at `path`:
/// the link's parent directory resolved through the filesystem
/// (symlinks, `..`, and redundant separators), joined with the link's own
/// file name. Registration and removal key a link through this one rule,
/// so any spelling of the same link finds the same row. The link
/// itself is not resolved: it is a file this store owns, not an alias.
///
/// A location that is not valid UTF-8 fails loudly instead of storing a
/// lossy path.
fn store_key(path: impl AsRef<Path>) -> Result<String, StorageError> {
    let path = path.as_ref();
    let absolute =
        std::path::absolute(path).map_err(|source| io_error(path.display().to_string(), source))?;
    let key = match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) => resolve(parent)?.join(name),
        // A filesystem root, or a path ending in `..`, names a directory.
        _ => resolve(&absolute)?,
    };
    key.to_str().map(str::to_owned).ok_or_else(|| {
        StorageError::backend(
            "worktree",
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "worktree path is not valid UTF-8",
            ),
        )
    })
}

/// The absolute `path` resolved through the filesystem as far as it
/// exists, with the missing tail appended as spelled. A removed world
/// folder thus still keys to what its registration resolved to, so its
/// stale row stays removable. A missing tail that steps out with `..`
/// cannot be appended faithfully, so the absolute spelling stands.
///
/// Windows spells a resolved path in its verbatim form (`\\?\C:\...`);
/// the prefix is dropped where the plain form names the same location,
/// so keys read the way users write paths.
fn resolve(path: impl AsRef<Path>) -> Result<PathBuf, StorageError> {
    let path = path.as_ref();
    let mut missing = Vec::new();
    let mut existing = path;
    loop {
        match std::fs::canonicalize(existing) {
            Ok(resolved) => {
                let resolved = without_verbatim_prefix(resolved);
                return Ok(missing
                    .iter()
                    .rev()
                    .fold(resolved, |acc, name| acc.join(name)));
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                match (existing.parent(), existing.file_name()) {
                    (Some(parent), Some(name)) => {
                        missing.push(name);
                        existing = parent;
                    }
                    _ => return Ok(path.to_path_buf()),
                }
            }
            Err(source) => return Err(io_error(existing.display().to_string(), source)),
        }
    }
}

fn without_verbatim_prefix(path: PathBuf) -> PathBuf {
    let plain = cfg!(windows)
        .then(|| path.to_str().and_then(plain_windows_spelling))
        .flatten();
    plain.map_or(path, PathBuf::from)
}

/// The plain spelling of a Windows verbatim path that has one:
/// `\\?\C:\x` is `C:\x` and `\\?\UNC\server\share` is `\\server\share`.
/// Other verbatim forms (device paths, `\\?\Volume{...}`) have none.
fn plain_windows_spelling(path: &str) -> Option<String> {
    if let Some(share) = path.strip_prefix(r"\\?\UNC\") {
        return Some(format!(r"\\{share}"));
    }
    let rest = path.strip_prefix(r"\\?\")?;
    let mut chars = rest.chars();
    let is_disk =
        chars.next().is_some_and(|c| c.is_ascii_alphabetic()) && chars.next() == Some(':');
    is_disk.then(|| rest.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_disk_and_unc_paths_have_plain_spellings() {
        assert_eq!(
            plain_windows_spelling(r"\\?\C:\worlds\a").as_deref(),
            Some(r"C:\worlds\a")
        );
        assert_eq!(
            plain_windows_spelling(r"\\?\UNC\server\share\a").as_deref(),
            Some(r"\\server\share\a")
        );
        assert_eq!(plain_windows_spelling(r"\\?\Volume{abc}\a"), None);
        assert_eq!(plain_windows_spelling(r"C:\worlds\a"), None);
    }
}
