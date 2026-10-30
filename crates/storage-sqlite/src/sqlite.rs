use std::path::Path;

use sekai_storage::{
    BackendKind, SekaiStoreOpener, StorageError, Store, StoreLockMode, io_error, parse_backend_url,
};

use crate::{SqliteBlob, SqliteMeta};

#[derive(Debug)]
pub struct SqliteStore {
    inner: Store<SqliteMeta, SqliteBlob>,
}

impl SqliteStore {
    pub const fn new(meta: SqliteMeta, blob: SqliteBlob) -> Self {
        Self {
            inner: Store::new(meta, blob),
        }
    }
}

impl sekai_storage::SekaiStore for SqliteStore {
    type Meta = SqliteMeta;
    type Blobs = SqliteBlob;
    type LockGuard = sekai_storage::StoreLock;

    fn meta(&self) -> &Self::Meta {
        self.inner.meta()
    }

    fn meta_mut(&mut self) -> &mut Self::Meta {
        self.inner.meta_mut()
    }

    fn blob(&self) -> &Self::Blobs {
        self.inner.blob()
    }

    fn blob_mut(&mut self) -> &mut Self::Blobs {
        self.inner.blob_mut()
    }

    fn blob_and_meta(&mut self) -> (&mut Self::Blobs, &Self::Meta) {
        self.inner.blob_and_meta()
    }

    fn lock(&self, mode: StoreLockMode) -> Result<Self::LockGuard, StorageError> {
        self.inner.lock(mode)
    }
}

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

impl SekaiStoreOpener for SqliteStore {
    async fn open(url: &str) -> Result<Self, StorageError> {
        let (kind, rest) = parse_backend_url(url)?;
        if kind != BackendKind::Sqlite {
            return Err(StorageError::UnsupportedBackend {
                url: url.to_owned(),
            });
        }
        open_sqlite(rest).await
    }

    async fn init(url: &str) -> Result<Self, StorageError> {
        let (kind, rest) = parse_backend_url(url)?;
        if kind != BackendKind::Sqlite {
            return Err(StorageError::UnsupportedBackend {
                url: url.to_owned(),
            });
        }
        init_sqlite(rest).await
    }
}
