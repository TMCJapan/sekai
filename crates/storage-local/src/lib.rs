//! Local storage backend for Sekai: SQLite metadata, sled blobs, and a
//! sled region-image cache.

mod blob;
mod cache;
mod meta;

pub use blob::SledBlob;
pub use cache::SledCache;
pub use meta::{SCHEMA_VERSION, SqliteMeta};

use std::future::Future;
use std::path::Path;

use sekai_storage::{SekaiStore, SekaiStoreOpener, StorageError, Store, io_error};

/// Local backend store handle: SQLite metadata, sled blobs, and a sled
/// region-image cache.
///
/// A newtype over the generic [`Store`] combinator rather than an alias:
/// the combinator belongs to `sekai-storage`, so only a local type can
/// carry this backend's trait implementations (orphan rule).
#[derive(Debug, Clone)]
pub struct LocalStore(Store<SqliteMeta, SledBlob, SledCache>);

impl LocalStore {
    /// Compose an opened store from its adapters.
    pub const fn new(meta: SqliteMeta, blob: SledBlob, cache: SledCache) -> Self {
        Self(Store::new(meta, blob, cache))
    }

    /// Metadata adapter.
    pub const fn meta(&self) -> &SqliteMeta {
        self.0.meta()
    }

    /// Mutable metadata adapter.
    pub const fn meta_mut(&mut self) -> &mut SqliteMeta {
        self.0.meta_mut()
    }

    /// Blob adapter.
    pub const fn blob(&self) -> &SledBlob {
        self.0.blob()
    }

    /// Mutable blob adapter.
    pub const fn blob_mut(&mut self) -> &mut SledBlob {
        self.0.blob_mut()
    }

    /// Cache adapter.
    pub const fn cache(&self) -> &SledCache {
        self.0.cache()
    }

    /// Mutable cache adapter.
    pub const fn cache_mut(&mut self) -> &mut SledCache {
        self.0.cache_mut()
    }
}

impl SekaiStore for LocalStore {
    type Meta = SqliteMeta;
    type Blobs = SledBlob;
    type Cache = SledCache;

    fn meta(&self) -> &Self::Meta {
        self.0.meta()
    }

    fn meta_mut(&mut self) -> &mut Self::Meta {
        self.0.meta_mut()
    }

    fn blob(&self) -> &Self::Blobs {
        self.0.blob()
    }

    fn blob_mut(&mut self) -> &mut Self::Blobs {
        self.0.blob_mut()
    }

    fn cache(&self) -> &Self::Cache {
        self.0.cache()
    }

    fn cache_mut(&mut self) -> &mut Self::Cache {
        self.0.cache_mut()
    }
}

impl SekaiStoreOpener for LocalStore {
    type Path = Path;

    fn open(
        path: impl AsRef<Self::Path>,
    ) -> impl Future<Output = Result<Self, StorageError>> + Send {
        // Own the path before the future is built: the trait promises a
        // `Send` future for every path holder, including `!Send` ones.
        let root = path.as_ref().to_path_buf();
        async move { open_local(root).await }
    }

    fn init(
        path: impl AsRef<Self::Path>,
    ) -> impl Future<Output = Result<Self, StorageError>> + Send {
        let root = path.as_ref().to_path_buf();
        async move { init_local(root).await }
    }
}

/// Initialize the store rooted at `root` and open it.
///
/// The layout is `<root>/meta.sqlite` plus the sled keyspaces at
/// `<root>/blobs` and `<root>/cache`; initializing an existing store again
/// is idempotent, like `git init`. This is the only path that materializes
/// a store, reserved for an explicit init (the CLI's `sekai init`). sled
/// admits a single opener per keyspace, so a concurrent initialization of
/// the same root fails loudly instead of racing.
pub async fn init_local(root: impl AsRef<Path>) -> Result<LocalStore, StorageError> {
    std::fs::create_dir_all(root.as_ref())
        .map_err(|source| io_error(root.as_ref().display().to_string(), source))?;
    connect(root).await
}

/// Open the existing store rooted at `root`.
///
/// The layout is `<root>/meta.sqlite` plus the sled keyspaces at
/// `<root>/blobs` and `<root>/cache`. Fails with
/// [`StorageError::StoreMissing`] when the store was never initialized,
/// without creating anything.
pub async fn open_local(root: impl AsRef<Path>) -> Result<LocalStore, StorageError> {
    if !markers_present(root.as_ref()) {
        return Err(StorageError::StoreMissing {
            path: root.as_ref().display().to_string(),
        });
    }
    connect(root).await
}

/// Whether `root` already holds an initialized store, checked without
/// opening (and locking) anything. Lets an explicit init tell a fresh
/// creation from a re-initialization.
pub fn local_store_exists(root: impl AsRef<Path>) -> bool {
    markers_present(root.as_ref())
}

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
async fn connect(root: impl AsRef<Path>) -> Result<LocalStore, StorageError> {
    let blob = SledBlob::open(root.as_ref().join("blobs")).await?;
    let cache = SledCache::open(root.as_ref().join("cache")).await?;
    let meta = SqliteMeta::open(root.as_ref().join("meta.sqlite")).await?;
    Ok(LocalStore::new(meta, blob, cache))
}
