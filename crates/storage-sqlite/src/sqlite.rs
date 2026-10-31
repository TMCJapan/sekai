use std::path::Path;

use sekai_storage::{StorageError, Store, io_error};

use crate::{SqliteBlob, SqliteMeta};

pub type SqliteStore = Store<SqliteMeta, SqliteBlob>;

/// Initialize the store rooted at `root`, creating it when missing.
///
/// The layout is `<root>/meta.sqlite` plus `<root>/blob.sqlite`; initializing
/// an existing store again is idempotent. This is the only path that
/// materializes a store, reserved for first-time store creation. It takes no
/// store lock:
/// schema creation serializes inside SQLite (see [`SqliteMeta::open`]), so
/// concurrent first initializations all succeed.
pub async fn init_sqlite(root: impl AsRef<Path>) -> Result<SqliteStore, StorageError> {
    std::fs::create_dir_all(root.as_ref()).map_err(|source| io_error(root.as_ref(), source))?;
    connect_sqlite(root).await
}

/// Open the existing store rooted at `root`.
///
/// The layout is `<root>/meta.sqlite` plus `<root>/blob.sqlite`. Fails with
/// [`StorageError::StoreMissing`] when the store was never initialized,
/// without creating anything.
pub async fn open_sqlite(root: impl AsRef<Path>) -> Result<SqliteStore, StorageError> {
    if !root.as_ref().join("meta.sqlite").is_file() || !root.as_ref().join("blob.sqlite").is_file()
    {
        return Err(StorageError::StoreMissing {
            path: root.as_ref().to_path_buf(),
        });
    }
    connect_sqlite(root).await
}

/// Attach blob and metadata adapters to an existing store root; the root
/// directory must already hold `meta.sqlite` and `blob.sqlite`.
async fn connect_sqlite(root: impl AsRef<Path>) -> Result<SqliteStore, StorageError> {
    let blob = SqliteBlob::open(&root.as_ref().join("blob.sqlite")).await?;
    let meta = SqliteMeta::open(&root.as_ref().join("meta.sqlite")).await?;
    Ok(SqliteStore::new(meta, blob))
}
