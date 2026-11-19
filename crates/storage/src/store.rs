//! High-level store boundaries implemented by concrete backends.

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
