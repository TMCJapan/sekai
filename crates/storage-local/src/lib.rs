//! Local storage backend for Sekai: SQLite metadata, sled blobs, and a
//! sled region-image cache.

mod blob;
mod cache;
mod meta;

pub use blob::SledBlob;
pub use cache::SledCache;
pub use meta::{SCHEMA_VERSION, SqliteMeta};

use std::path::Path;

use sekai_storage::{SekaiStore, SekaiStoreOpener, StorageError, io_error};

#[derive(Debug)]
pub struct LocalStore {
    meta: SqliteMeta,
    blob: SledBlob,
    cache: SledCache,
}

impl SekaiStore for LocalStore {
    type Meta = SqliteMeta;
    type Blobs = SledBlob;
    type Cache = SledCache;
    fn meta(&self) -> &Self::Meta {
        &self.meta
    }
    fn meta_mut(&mut self) -> &mut Self::Meta {
        &mut self.meta
    }
    fn blob(&self) -> &Self::Blobs {
        &self.blob
    }
    fn blob_mut(&mut self) -> &mut Self::Blobs {
        &mut self.blob
    }
    fn cache(&self) -> &Self::Cache {
        &self.cache
    }
    fn cache_mut(&mut self) -> &mut Self::Cache {
        &mut self.cache
    }
}

impl SekaiStoreOpener for LocalStore {
    type Path = Path;

    /// Initialize the store rooted at `root` and open it.
    ///
    /// The layout is `<root>/meta.sqlite` plus the sled keyspaces at
    /// `<root>/blobs` and `<root>/cache`; initializing an existing store again
    /// is idempotent, like `git init`. This is the only path that materializes
    /// a store, reserved for an explicit init (the CLI's `sekai init`). sled
    /// admits a single opener per keyspace, so a concurrent initialization of
    /// the same root fails loudly instead of racing.
    async fn init(path: impl AsRef<Self::Path>) -> Result<Self, StorageError> {
        std::fs::create_dir_all(path.as_ref())
            .map_err(|source| io_error(path.as_ref().display().to_string(), source))?;
        Self::connect(path).await
    }

    /// Open the existing store rooted at `root`.
    ///
    /// The layout is `<root>/meta.sqlite` plus the sled keyspaces at
    /// `<root>/blobs` and `<root>/cache`. Fails with
    /// [`StorageError::StoreMissing`] when the store was never initialized,
    /// without creating anything.
    async fn open(path: impl AsRef<Self::Path>) -> Result<Self, StorageError> {
        if !Self::markers_present(path.as_ref()) {
            return Err(StorageError::StoreMissing {
                path: path.as_ref().display().to_string(),
            });
        }
        Self::connect(path).await
    }
}

/// Whether `root` already holds an initialized store, checked without
/// opening (and locking) anything. Lets an explicit init tell a fresh
/// creation from a re-initialization.
pub fn local_store_exists(root: impl AsRef<Path>) -> bool {
    LocalStore::markers_present(root.as_ref())
}

impl LocalStore {
    /// `conf` is the config file sled writes on first open: each keyspace's
    /// existence marker, checked without opening (and locking) anything.
    fn markers_present(root: impl AsRef<Path>) -> bool {
        let root = root.as_ref();
        root.join("meta.sqlite").is_file()
            && root.join("blobs/conf").is_file()
            && root.join("cache/conf").is_file()
    }
    /// Attach the metadata, blob, and cache adapters to an existing store root;
    /// the root directory must already hold `meta.sqlite` and the `blobs` and
    /// `cache` keyspaces.
    async fn connect(root: impl AsRef<Path>) -> Result<Self, StorageError> {
        let blob = SledBlob::open(root.as_ref().join("blobs")).await?;
        let cache = SledCache::open(root.as_ref().join("cache")).await?;
        let meta = SqliteMeta::open(root.as_ref().join("meta.sqlite")).await?;
        Ok(Self { meta, blob, cache })
    }
}
