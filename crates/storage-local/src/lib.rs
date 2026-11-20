//! Local storage backend for Sekai: SQLite metadata, sled blobs, and a
//! sled region-image cache.

mod blob;
mod cache;
mod config;
mod meta;

pub use blob::SledBlob;
pub use cache::SledCache;
pub use config::{CONFIG_VERSION, Config};
pub use meta::SqliteMeta;

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
    /// The layout is `<root>/version` plus `<root>/meta.sqlite` and the sled
    /// keyspaces at `<root>/blobs` and `<root>/cache`; initializing an existing
    /// store again is idempotent, like `git init`. This is the only path that
    /// materializes a store, reserved for an explicit init (the CLI's
    /// `sekai init`). The version file is read and the store is gated before
    /// any database opens. sled admits a single opener per keyspace, so a concurrent
    /// initialization of the same root fails loudly instead of racing.
    async fn init(path: impl AsRef<Self::Path>) -> Result<Self, StorageError> {
        let root = path.as_ref();
        std::fs::create_dir_all(root)
            .map_err(|source| io_error(root.display().to_string(), source))?;
        config::init(root)?;
        Self::connect(root).await
    }

    /// Open the existing store rooted at `root`.
    ///
    /// The layout is `<root>/version` plus `<root>/meta.sqlite` and the sled
    /// keyspaces at `<root>/blobs` and `<root>/cache`. The version file is read
    /// and the store is gated before any database opens. Fails with
    /// [`StorageError::StoreMissing`] when the store was never initialized,
    /// without creating anything.
    async fn open(path: impl AsRef<Self::Path>) -> Result<Self, StorageError> {
        let root = path.as_ref();
        if !Self::markers_present(root) {
            return Err(StorageError::StoreMissing {
                path: root.display().to_string(),
            });
        }
        Config::read(root)?;
        Self::connect(root).await
    }
}

/// Whether `root` already holds an initialized store, checked without
/// opening (and locking) anything. Lets an explicit init tell a fresh
/// creation from a re-initialization.
pub fn local_store_exists(root: impl AsRef<Path>) -> bool {
    LocalStore::markers_present(root.as_ref())
}

impl LocalStore {
    /// Each component's existence marker, checked without opening (and
    /// locking) anything: `version`, `meta.sqlite`, and the `conf` files
    /// sled writes inside each keyspace on first open.
    fn markers_present(root: impl AsRef<Path>) -> bool {
        let root = root.as_ref();
        root.join(config::FILE_NAME).is_file()
            && root.join("meta.sqlite").is_file()
            && root.join("blobs/conf").is_file()
            && root.join("cache/conf").is_file()
    }
    /// Attach the metadata, blob, and cache adapters to an existing store root;
    /// the root directory must already hold `version`, `meta.sqlite`, and the
    /// `blobs` and `cache` keyspaces.
    async fn connect(root: impl AsRef<Path>) -> Result<Self, StorageError> {
        let blob = SledBlob::open(root.as_ref().join("blobs")).await?;
        let cache = SledCache::open(root.as_ref().join("cache")).await?;
        let meta = SqliteMeta::open(root.as_ref().join("meta.sqlite")).await?;
        Ok(Self { meta, blob, cache })
    }
}
