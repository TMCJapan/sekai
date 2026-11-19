//! High-level store boundaries implemented by concrete backends.

use core::future::Future;

use crate::{BlobStore, CacheStore, MetaStore};

use crate::api::{StorageError, Store};

/// One opened store: metadata, blobs, the cache, and the control operations
/// around them.
///
/// A backend implements this through [`Store`] (metadata + blob + cache
/// adapters), so a consumer such as `SekaiInstance` never names a concrete
/// database or blob store.
pub trait SekaiStore: Send + Sync + Sized + 'static {
    /// Metadata adapter.
    type Meta: MetaStore<Error = StorageError> + Send + Sync;

    /// Blob adapter.
    ///
    /// Cloned freely into concurrent ingest workers: the CAS is append-only,
    /// so writers never exclude each other.
    type Blobs: BlobStore<Error = StorageError> + Clone + Send + Sync + 'static;

    /// Region-image cache adapter.
    ///
    /// Derived state, cloned like the blob adapter: concurrent workers may
    /// populate it, and a missing entry only costs performance.
    type Cache: CacheStore<Error = StorageError> + Clone + Send + Sync + 'static;

    /// Metadata adapter.
    fn meta(&self) -> &Self::Meta;

    /// Mutable metadata adapter.
    fn meta_mut(&mut self) -> &mut Self::Meta;

    /// Blob adapter.
    fn blob(&self) -> &Self::Blobs;

    /// Mutable blob adapter.
    fn blob_mut(&mut self) -> &mut Self::Blobs;

    /// Cache adapter.
    fn cache(&self) -> &Self::Cache;

    /// Mutable cache adapter.
    fn cache_mut(&mut self) -> &mut Self::Cache;
}

/// Open or initialize a store of one concrete backend.
///
/// The two entry points are exclusive: only [`Self::init`] materializes a
/// store, while [`Self::open`] never creates anything. The backend owns
/// its location type [`Self::Path`]; the local backend reads it as a store
/// root directory. The associated type keeps this crate `no_std`: `Path`
/// itself never appears here, only whatever the backend names.
pub trait SekaiStoreOpener: SekaiStore {
    /// The backend's location type, borrowed through [`AsRef`] at the entry
    /// points (`std::path::Path` for the local backend, a string or URL
    /// type for a remote one).
    type Path: ?Sized;

    /// Open the existing store named by `path`.
    ///
    /// Fails with [`StorageError::StoreMissing`] when the store was never
    /// initialized, without creating anything.
    fn open(path: impl AsRef<Self::Path>) -> impl Future<Output = Result<Self, StorageError>>;

    /// Initialize the store named by `path` and open it.
    ///
    /// The only path that materializes a store, reserved for an explicit
    /// init (the CLI's `sekai init`). Initializing an existing store again
    /// is idempotent, like `git init`.
    fn init(path: impl AsRef<Self::Path>) -> impl Future<Output = Result<Self, StorageError>>;
}

impl<M, C, K> SekaiStore for Store<M, C, K>
where
    M: MetaStore<Error = StorageError> + Clone + Send + Sync + 'static,
    C: BlobStore<Error = StorageError> + Clone + Send + Sync + 'static,
    K: CacheStore<Error = StorageError> + Clone + Send + Sync + 'static,
{
    type Meta = M;
    type Blobs = C;
    type Cache = K;

    fn meta(&self) -> &Self::Meta {
        Self::meta(self)
    }

    fn meta_mut(&mut self) -> &mut Self::Meta {
        Self::meta_mut(self)
    }

    fn blob(&self) -> &Self::Blobs {
        Self::blob(self)
    }

    fn blob_mut(&mut self) -> &mut Self::Blobs {
        Self::blob_mut(self)
    }

    fn cache(&self) -> &Self::Cache {
        Self::cache(self)
    }

    fn cache_mut(&mut self) -> &mut Self::Cache {
        Self::cache_mut(self)
    }
}
