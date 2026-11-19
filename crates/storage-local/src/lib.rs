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
    /// The layout is `<root>/conf.toml` plus `<root>/meta.sqlite` and the sled
    /// keyspaces at `<root>/blobs` and `<root>/cache`; initializing an existing
    /// store again is idempotent, like `git init`. This is the only path that
    /// materializes a store, reserved for an explicit init (the CLI's
    /// `sekai init`). The config is read and version-gated before any database
    /// opens. When it names a `main_worktree`, the databases live in that
    /// shared store root instead: init materializes it there (keeping any
    /// existing config) while `root` keeps only its pointer config. sled
    /// admits a single opener per keyspace, so a concurrent initialization of
    /// the same root fails loudly instead of racing.
    async fn init(path: impl AsRef<Self::Path>) -> Result<Self, StorageError> {
        let root = path.as_ref();
        std::fs::create_dir_all(root)
            .map_err(|source| io_error(root.display().to_string(), source))?;
        let config = config::init(root)?;
        let data_root = config.data_root(root);
        if config.main_worktree.is_some() {
            std::fs::create_dir_all(&data_root)
                .map_err(|source| io_error(data_root.display().to_string(), source))?;
            config::init(&data_root)?;
        }
        Self::connect(&data_root).await
    }

    /// Open the existing store rooted at `root`.
    ///
    /// The layout is `<root>/conf.toml` plus `<root>/meta.sqlite` and the sled
    /// keyspaces at `<root>/blobs` and `<root>/cache`. The config is read and
    /// version-gated before any database opens; when it names a `main_worktree`,
    /// the databases resolve from that shared store root instead, so every
    /// worktree opens the same store. Fails with [`StorageError::StoreMissing`]
    /// when the store was never initialized, without creating anything.
    async fn open(path: impl AsRef<Self::Path>) -> Result<Self, StorageError> {
        let root = path.as_ref();
        if !root.join(config::FILE_NAME).is_file() {
            return Err(StorageError::StoreMissing {
                path: root.display().to_string(),
            });
        }
        let config = Config::read(root)?;
        let data_root = config.data_root(root);
        if !Self::markers_present(&data_root) {
            return Err(StorageError::StoreMissing {
                path: data_root.display().to_string(),
            });
        }
        Self::connect(&data_root).await
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
    /// locking) anything: `conf.toml`, `meta.sqlite`, and the `conf` files
    /// sled writes inside each keyspace on first open.
    fn markers_present(root: impl AsRef<Path>) -> bool {
        let root = root.as_ref();
        root.join(config::FILE_NAME).is_file()
            && root.join("meta.sqlite").is_file()
            && root.join("blobs/conf").is_file()
            && root.join("cache/conf").is_file()
    }
    /// Attach the metadata, blob, and cache adapters to the store root holding
    /// the databases - the config's `main_worktree` target when set, else the
    /// root holding the config. The directory must already hold `conf.toml`,
    /// `meta.sqlite`, and the `blobs` and `cache` keyspaces.
    async fn connect(root: impl AsRef<Path>) -> Result<Self, StorageError> {
        let blob = SledBlob::open(root.as_ref().join("blobs")).await?;
        let cache = SledCache::open(root.as_ref().join("cache")).await?;
        let meta = SqliteMeta::open(root.as_ref().join("meta.sqlite")).await?;
        Ok(Self { meta, blob, cache })
    }
}
