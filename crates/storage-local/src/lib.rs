//! Local storage backend for Sekai: SQLite metadata plus sled blobs.

mod blob;
mod meta;

pub use blob::SledBlob;
pub use meta::{SCHEMA_VERSION, SqliteMeta};

use std::path::Path;

use sekai_storage::{StorageError, Store, io_error};

pub type LocalStore = Store<SqliteMeta, SledBlob>;

/// Initialize the store rooted at `root`, creating it when missing.
///
/// The layout is `<root>/meta.sqlite` plus the sled keyspace at
/// `<root>/blobs`; initializing an existing store again is idempotent. This
/// is the only path that materializes a store, reserved for first-time store
/// creation. sled admits a single opener per keyspace, so a concurrent
/// initialization of the same root fails loudly instead of racing.
pub async fn init_sqlite(root: impl AsRef<Path>) -> Result<LocalStore, StorageError> {
    std::fs::create_dir_all(root.as_ref()).map_err(|source| io_error(root.as_ref(), source))?;
    connect_sqlite(root).await
}

/// Open the existing store rooted at `root`.
///
/// The layout is `<root>/meta.sqlite` plus the sled keyspace at
/// `<root>/blobs`. Fails with [`StorageError::StoreMissing`] when the store
/// was never initialized, without creating anything.
pub async fn open_sqlite(root: impl AsRef<Path>) -> Result<LocalStore, StorageError> {
    // `conf` is the config file sled writes on first open: the keyspace's
    // existence marker, checked without opening (and locking) anything.
    if !root.as_ref().join("meta.sqlite").is_file() || !root.as_ref().join("blobs/conf").is_file() {
        return Err(StorageError::StoreMissing {
            path: root.as_ref().to_path_buf(),
        });
    }
    connect_sqlite(root).await
}

/// Attach blob and metadata adapters to an existing store root; the root
/// directory must already hold `meta.sqlite` and the `blobs` keyspace.
async fn connect_sqlite(root: impl AsRef<Path>) -> Result<LocalStore, StorageError> {
    let blob = SledBlob::open(root.as_ref().join("blobs")).await?;
    let meta = SqliteMeta::open(root.as_ref().join("meta.sqlite")).await?;
    Ok(LocalStore::new(meta, blob))
}
