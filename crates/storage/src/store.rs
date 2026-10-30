//! High-level store boundaries implemented by concrete backends.

use core::future::Future;
use std::vec::Vec;

use sekai_core::{BlobHash, BlobStore, MetaStore};

use crate::api::{StorageError, Store};
use crate::lock::StoreLockMode;

/// One opened store: metadata, blobs, and the control operations around them.
///
/// A backend implements this through [`Store`] (metadata + blob adapters), so a
/// consumer such as `SekaiInstance` never names a concrete database or blob store.
pub trait SekaiStore: Send + Sync + Sized + 'static {
    /// Metadata adapter.
    type Meta: MetaStore<Error = StorageError> + Send + Sync;

    /// Blob adapter.
    type Blobs: BlockingBlobStore;

    /// Guard held while an operation holds the store.
    ///
    /// The guard crosses threads: an operation shares it into the blocking
    /// tasks that touch files, so a cancelled future cannot release the
    /// store while those tasks still run.
    type LockGuard: Send + Sync;

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

    /// Lock the store for the duration of an operation.
    ///
    /// The guard keeps other operations out until it drops: see
    /// [`StoreLockMode`] for what each mode allows.
    fn lock(&self, mode: StoreLockMode) -> Result<Self::LockGuard, StorageError>;
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

/// Blob operations that must run inside blocking workers.
///
/// These mirror [`BlobStore`] but are synchronous: commit ingest hashes and
/// stores chunks from a blocking pool, while metadata operations stay async.
/// A backend that cannot offer blocking writes cannot run the current ingest
/// pipeline.
pub trait BlockingBlobStore:
    BlobStore<Error = StorageError> + Clone + Send + Sync + 'static
{
    /// Guard held while an operation holds the store.
    type LockGuard: Send + Sync;

    /// Blocking equivalent of [`BlobStore::contains`].
    fn contains_blob(&self, hash: &BlobHash) -> Result<bool, StorageError>;

    /// Blocking equivalent of [`BlobStore::put`].
    fn put_blob(&mut self, hash: &BlobHash, payload: &[u8]) -> Result<bool, StorageError>;

    /// Blocking equivalent of [`BlobStore::fetch_into`].
    fn fetch_blob(&self, hash: &BlobHash, out: &mut Vec<u8>) -> Result<(), StorageError>;

    /// Make blob writes durable before metadata commit.
    fn sync_dirs(&mut self) -> Result<(), StorageError>;

    /// Lock the store for the duration of an operation.
    ///
    /// The lock is held in the store root, so it excludes operations
    /// against the same store on the same host. See [`StoreLockMode`] for
    /// what each mode allows.
    fn lock(&self, mode: StoreLockMode) -> Result<Self::LockGuard, StorageError>;
}

impl<M, C> SekaiStore for Store<M, C>
where
    M: MetaStore<Error = StorageError> + Clone + Send + Sync + 'static,
    C: BlockingBlobStore,
{
    type Meta = M;
    type Blobs = C;
    type LockGuard = C::LockGuard;

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

    fn lock(&self, mode: StoreLockMode) -> Result<Self::LockGuard, StorageError> {
        self.blob().lock(mode)
    }
}
