//! High-level store boundaries implemented by concrete backends.

use core::future::Future;

use sekai_core::{BlobStore, MetaStore};

use crate::api::{StorageError, Store};

/// One opened store: metadata, blobs, and the control operations around them.
///
/// A backend implements this through [`Store`] (metadata + blob adapters), so a
/// consumer such as `SekaiInstance` never names a concrete database or CAS.
pub trait SekaiStore: Send + Sync + Sized + 'static {
    /// Metadata adapter.
    type Meta: MetaStore<Error = StorageError> + Send + Sync;

    /// Blob adapter.
    ///
    /// Cloned freely into concurrent ingest workers: the CAS is append-only,
    /// so writers never exclude each other.
    type Blobs: BlobStore<Error = StorageError> + Clone + Send + Sync + 'static;

    /// Metadata adapter.
    fn meta(&self) -> &Self::Meta;

    /// Mutable metadata adapter.
    fn meta_mut(&mut self) -> &mut Self::Meta;

    /// Blob adapter.
    fn cas(&self) -> &Self::Blobs;

    /// Mutable blob adapter.
    fn cas_mut(&mut self) -> &mut Self::Blobs;

    /// Borrow both adapters for operations that need simultaneous access.
    fn cas_and_meta(&mut self) -> (&mut Self::Blobs, &Self::Meta);
}

/// Open or initialize a store of one concrete backend.
///
/// The implementation owns its URL grammar: it recognizes the schemes that
/// name it and rejects the others with
/// [`StorageError::UnsupportedBackend`].
pub trait SekaiStoreOpener: SekaiStore {
    /// Open the existing store named by `url`.
    fn open(url: &str) -> impl Future<Output = Result<Self, StorageError>> + Send;

    /// Open the store named by `url`, creating it when missing.
    fn init(url: &str) -> impl Future<Output = Result<Self, StorageError>> + Send;
}

impl<M, C> SekaiStore for Store<M, C>
where
    M: MetaStore<Error = StorageError> + Clone + Send + Sync + 'static,
    C: BlobStore<Error = StorageError> + Clone + Send + Sync + 'static,
{
    type Meta = M;
    type Blobs = C;

    fn meta(&self) -> &Self::Meta {
        Self::meta(self)
    }

    fn meta_mut(&mut self) -> &mut Self::Meta {
        Self::meta_mut(self)
    }

    fn cas(&self) -> &Self::Blobs {
        Self::cas(self)
    }

    fn cas_mut(&mut self) -> &mut Self::Blobs {
        Self::cas_mut(self)
    }

    fn cas_and_meta(&mut self) -> (&mut Self::Blobs, &Self::Meta) {
        Self::cas_and_meta(self)
    }
}
