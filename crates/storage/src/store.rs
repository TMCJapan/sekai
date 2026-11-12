//! High-level store boundaries implemented by concrete backends.

use core::future::Future;

use sekai_core::{BlobStore, MetaStore};

use crate::api::{StorageError, Store};

/// One opened store: metadata, blobs, and the control operations around them.
///
/// A backend implements this through [`Store`] (metadata + blob adapters), so a
/// consumer such as `SekaiInstance` never names a concrete database or blob store.
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
    fn blob(&self) -> &Self::Blobs;

    /// Mutable blob adapter.
    fn blob_mut(&mut self) -> &mut Self::Blobs;

    /// Borrow both adapters for operations that need simultaneous access.
    fn blob_and_meta(&mut self) -> (&mut Self::Blobs, &Self::Meta);
}

/// Open or initialize a store of one concrete backend.
///
/// The two entry points are exclusive: only [`Self::init`] materializes a
/// store, while [`Self::open`] never creates anything. The implementation
/// owns the meaning of the location string; the local backend reads it as
/// a store root directory.
pub trait SekaiStoreOpener: SekaiStore {
    /// Open the existing store named by `location`.
    ///
    /// Fails with [`StorageError::StoreMissing`] when the store was never
    /// initialized, without creating anything.
    fn open(location: &str) -> impl Future<Output = Result<Self, StorageError>> + Send;

    /// Initialize the store named by `location` and open it.
    ///
    /// The only path that materializes a store, reserved for an explicit
    /// init (the CLI's `sekai init`). Initializing an existing store again
    /// is idempotent, like `git init`.
    fn init(location: &str) -> impl Future<Output = Result<Self, StorageError>> + Send;
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

    fn blob(&self) -> &Self::Blobs {
        Self::blob(self)
    }

    fn blob_mut(&mut self) -> &mut Self::Blobs {
        Self::blob_mut(self)
    }

    fn blob_and_meta(&mut self) -> (&mut Self::Blobs, &Self::Meta) {
        Self::blob_and_meta(self)
    }
}
