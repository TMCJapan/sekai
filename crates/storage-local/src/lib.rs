//! Local storage backend for Sekai: SQLite metadata, sled blobs, and a
//! sled region-image cache.

mod blob;
mod cache;
pub mod config;
mod meta;
pub mod version;
pub mod worktree;

// ast-grep-ignore: no-pub-use
pub use blob::SledBlob;
// ast-grep-ignore: no-pub-use
pub use cache::SledCache;
// ast-grep-ignore: no-pub-use
pub use meta::SqliteMeta;

use std::path::{Path, PathBuf};

use sekai_storage::{SekaiStore, SekaiStoreOpener, StorageError, io_error};

#[derive(Debug)]
pub struct LocalStore {
    root: PathBuf,
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
    /// `sekai init`). The version file is read and the store is gated
    /// before any database opens. sled admits a single opener per keyspace,
    /// so a concurrent initialization of the same root fails loudly instead
    /// of racing.
    ///
    /// The store registers itself as its main worktree, keyed by the
    /// absolute root with HEAD unborn on the default branch; a re-init
    /// keeps that row and its HEAD.
    async fn init(path: impl AsRef<Self::Path>) -> Result<Self, StorageError> {
        let root = path.as_ref();
        std::fs::create_dir_all(root)
            .map_err(|source| io_error(root.display().to_string(), source))?;
        version::init(root)?;
        let store = Self::connect(root, root).await?;
        store.init_worktree().await?;
        Ok(store)
    }

    /// Open the existing store rooted at `root`.
    ///
    /// The layout is `<root>/version` plus `<root>/meta.sqlite` and the sled
    /// keyspaces at `<root>/blobs` and `<root>/cache`. The version file is read
    /// and the store is gated before any database opens. Fails with
    /// [`StorageError::StoreMissing`] when the store was never initialized,
    /// without creating anything.
    ///
    /// The location may also be a plain file instead of a directory: a store
    /// link whose content names the real root, so several worlds can share
    /// one store. Only open follows a link; init never does.
    ///
    /// The location itself, not the root it resolves to, is the worktree
    /// whose HEAD this store commits on: a link opens its linked worktree,
    /// the root the store's own.
    async fn open(path: impl AsRef<Self::Path>) -> Result<Self, StorageError> {
        let root = Self::resolve_root(path.as_ref())?;
        if !Self::markers_present(&root) {
            return Err(StorageError::StoreMissing {
                path: root.display().to_string(),
            });
        }
        version::check(&root)?;
        Self::connect(&root, path.as_ref()).await
    }
}

/// Whether `root` already holds an initialized store, checked without
/// opening (and locking) anything. Lets an explicit init tell a fresh
/// creation from a re-initialization.
pub fn local_store_exists(root: impl AsRef<Path>) -> bool {
    LocalStore::markers_present(root.as_ref())
}

impl LocalStore {
    /// The absolute root this handle opened: the key of the store's own
    /// (main) worktree row, so a caller can tell it from the linked ones.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve the location the caller named to the store root.
    ///
    /// A directory (or a path that does not exist) is the root itself. A
    /// plain file is a store link: its content is the real root's path, so
    /// several worlds can share one store without moving it. A relative
    /// target resolves against the link's parent directory, keeping the
    /// link valid when the whole folder moves; the link is followed once,
    /// never chained.
    fn resolve_root(path: impl AsRef<Path>) -> Result<PathBuf, StorageError> {
        let path = path.as_ref();
        if !path.is_file() {
            return Ok(path.to_path_buf());
        }
        let text = std::fs::read_to_string(path)
            .map_err(|source| io_error(path.display().to_string(), source))?;
        let target = text.trim();
        if target.is_empty() {
            return Err(StorageError::backend(
                "store link",
                std::io::Error::new(std::io::ErrorKind::InvalidData, "empty store link file"),
            ));
        }
        let target = Path::new(target);
        let root = if target.is_absolute() {
            target.to_path_buf()
        } else {
            path.parent().unwrap_or_else(|| Path::new(".")).join(target)
        };
        Ok(root)
    }

    /// Each component's existence marker, checked without opening (and
    /// locking) anything: `version`, `meta.sqlite`, and the `conf` files
    /// sled writes inside each keyspace on first open.
    fn markers_present(root: impl AsRef<Path>) -> bool {
        let root = root.as_ref();
        root.join(version::FILE_NAME).is_file()
            && root.join("meta.sqlite").is_file()
            && root.join("blobs/conf").is_file()
            && root.join("cache/conf").is_file()
    }
    /// Attach the metadata, blob, and cache adapters to an existing store root;
    /// the root directory must already hold `version`, `meta.sqlite`, and the
    /// `blobs` and `cache` keyspaces. The root is made absolute, not canonical:
    /// worktree links name it from any working directory, and the absolute
    /// spelling is the identity every worktree key shares (`store_key`).
    /// `opened` is the location the caller named, the worktree the
    /// metadata commits on.
    async fn connect(
        root: impl AsRef<Path>,
        opened: impl AsRef<Path>,
    ) -> Result<Self, StorageError> {
        let root = std::path::absolute(root.as_ref())
            .map_err(|source| io_error(root.as_ref().display().to_string(), source))?;
        let blob = SledBlob::open(root.join("blobs")).await?;
        let cache = SledCache::open(root.join("cache")).await?;
        let meta = SqliteMeta::open(root.join("meta.sqlite"), opened).await?;
        Ok(Self {
            root,
            meta,
            blob,
            cache,
        })
    }
}
