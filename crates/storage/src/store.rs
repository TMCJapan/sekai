//! High-level store boundaries implemented by concrete backends.

use core::future::Future;
use std::vec::Vec;

use sekai_core::{
    ApplyOutcome, BlobHash, BlobStore, ChunkCoord, ChunkHistoryEntry, DiffHash, FoldOutcome,
    MetaStore, RegionFingerprint, RegionKey, RegionStateEntry, Snapshot, SnapshotEntry, SnapshotId,
    SnapshotTag, TagName,
};

use crate::api::{StorageError, Store};
use crate::lock::StoreLockMode;

/// One opened store: metadata, blobs, and the control operations around them.
///
/// A backend implements this through [`Store`] (metadata + blob adapters), so a
/// consumer such as `SekaiInstance` never names a concrete database or CAS.
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
    fn cas(&self) -> &Self::Blobs;

    /// Mutable blob adapter.
    fn cas_mut(&mut self) -> &mut Self::Blobs;

    /// Borrow both adapters for operations that need simultaneous access.
    fn cas_and_meta(&mut self) -> (&mut Self::Blobs, &Self::Meta);

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
/// These mirror [`BlobStore`] but are synchronous: backup ingest hashes and
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

    fn cas(&self) -> &Self::Blobs {
        Self::cas(self)
    }

    fn cas_mut(&mut self) -> &mut Self::Blobs {
        Self::cas_mut(self)
    }

    fn cas_and_meta(&mut self) -> (&mut Self::Blobs, &Self::Meta) {
        Self::cas_and_meta(self)
    }

    fn lock(&self, mode: StoreLockMode) -> Result<Self::LockGuard, StorageError> {
        self.cas().lock(mode)
    }
}

/// Store used when no backend was compiled in.
///
/// Constructors fail loudly with [`StorageError::UnsupportedBackend`]; the
/// type only exists so generic consumers can still be instantiated.
///
/// It is neither `Default` nor `Copy`: an unavailable store has to be
/// named explicitly, and a value must never be conjured into existence
/// behind the "fails loudly" contract.
#[derive(Debug, Clone)]
pub struct UnavailableStore {
    meta: UnavailableMeta,
    blobs: UnavailableBlobs,
}

/// Metadata adapter of [`UnavailableStore`].
#[derive(Debug, Clone)]
pub struct UnavailableMeta;

/// Blob adapter of [`UnavailableStore`].
#[derive(Debug, Clone)]
pub struct UnavailableBlobs;

fn unavailable<T>() -> Result<T, StorageError> {
    Err(StorageError::UnsupportedBackend {
        url: "no storage backend compiled".to_owned(),
    })
}

impl MetaStore for UnavailableMeta {
    type Error = StorageError;

    fn create_snapshot(
        &mut self,
        _created_at_ms: u64,
    ) -> impl Future<Output = Result<SnapshotId, StorageError>> + Send {
        core::future::ready(unavailable())
    }

    fn record_chunk(
        &mut self,
        _snapshot: SnapshotId,
        _coord: &ChunkCoord,
        _blob: Option<&BlobHash>,
        _diff: Option<&DiffHash>,
    ) -> impl Future<Output = Result<(), StorageError>> + Send {
        core::future::ready(unavailable())
    }

    fn lookup_chunk(
        &self,
        _snapshot: SnapshotId,
        _coord: &ChunkCoord,
    ) -> impl Future<Output = Result<Option<ChunkHistoryEntry>, StorageError>> + Send {
        core::future::ready(unavailable())
    }

    fn lookup_snapshot(
        &self,
        _id: SnapshotId,
    ) -> impl Future<Output = Result<Option<Snapshot>, StorageError>> + Send {
        core::future::ready(unavailable())
    }

    fn latest_snapshot(
        &self,
    ) -> impl Future<Output = Result<Option<Snapshot>, StorageError>> + Send {
        core::future::ready(unavailable())
    }

    fn visit_snapshot_chunks<F>(
        &self,
        _snapshot: SnapshotId,
        _visit: F,
    ) -> impl Future<Output = Result<(), StorageError>> + Send
    where
        F: FnMut(&ChunkHistoryEntry) -> bool + Send,
    {
        core::future::ready(unavailable())
    }

    fn visit_fresh_rows<F>(
        &self,
        _snapshot: SnapshotId,
        _visit: F,
    ) -> impl Future<Output = Result<(), StorageError>> + Send
    where
        F: FnMut(&ChunkHistoryEntry) -> bool + Send,
    {
        core::future::ready(unavailable())
    }

    fn visit_snapshots<F>(&self, _visit: F) -> impl Future<Output = Result<(), StorageError>> + Send
    where
        F: FnMut(&Snapshot) -> bool + Send,
    {
        core::future::ready(unavailable())
    }

    fn load_region_states(
        &self,
    ) -> impl Future<Output = Result<Vec<RegionStateEntry>, StorageError>> + Send {
        core::future::ready(unavailable())
    }

    fn apply_snapshot_incremental(
        &mut self,
        _created_at_ms: u64,
        _entries: &[SnapshotEntry],
        _carry_from: Option<(SnapshotId, &[RegionKey])>,
        _fingerprints: &[RegionFingerprint],
        _removed: &[RegionKey],
    ) -> impl Future<Output = Result<ApplyOutcome, StorageError>> + Send {
        core::future::ready(unavailable())
    }

    fn tag_snapshot(
        &mut self,
        _name: &TagName,
        _snapshot: SnapshotId,
        _created_at_ms: u64,
    ) -> impl Future<Output = Result<(), StorageError>> + Send {
        core::future::ready(unavailable())
    }

    fn untag(
        &mut self,
        _name: &TagName,
    ) -> impl Future<Output = Result<bool, StorageError>> + Send {
        core::future::ready(unavailable())
    }

    fn lookup_tag(
        &self,
        _name: &TagName,
    ) -> impl Future<Output = Result<Option<SnapshotTag>, StorageError>> + Send {
        core::future::ready(unavailable())
    }

    fn visit_tags<F>(&self, _visit: F) -> impl Future<Output = Result<(), StorageError>> + Send
    where
        F: FnMut(&SnapshotTag) -> bool + Send,
    {
        core::future::ready(unavailable())
    }

    fn retire_snapshot(
        &mut self,
        _from: SnapshotId,
        _into: SnapshotId,
    ) -> impl Future<Output = Result<FoldOutcome, StorageError>> + Send {
        core::future::ready(unavailable())
    }
}

impl BlobStore for UnavailableBlobs {
    type Error = StorageError;

    fn contains(
        &self,
        _hash: &BlobHash,
    ) -> impl Future<Output = Result<bool, StorageError>> + Send {
        core::future::ready(unavailable())
    }

    fn put(
        &mut self,
        _hash: &BlobHash,
        _payload: &[u8],
    ) -> impl Future<Output = Result<bool, StorageError>> + Send {
        core::future::ready(unavailable())
    }

    fn sync(&mut self) -> impl Future<Output = Result<(), StorageError>> + Send {
        core::future::ready(unavailable())
    }

    fn fetch_into(
        &self,
        _hash: &BlobHash,
        _out: &mut Vec<u8>,
    ) -> impl Future<Output = Result<(), StorageError>> + Send {
        core::future::ready(unavailable())
    }

    fn remove(
        &mut self,
        _hash: &BlobHash,
    ) -> impl Future<Output = Result<bool, StorageError>> + Send {
        core::future::ready(unavailable())
    }

    fn visit_blobs<F>(&self, _visit: F) -> impl Future<Output = Result<(), StorageError>> + Send
    where
        F: FnMut(&BlobHash) -> bool + Send,
    {
        core::future::ready(unavailable())
    }
}

impl BlockingBlobStore for UnavailableBlobs {
    type LockGuard = ();

    fn contains_blob(&self, _hash: &BlobHash) -> Result<bool, StorageError> {
        unavailable()
    }

    fn put_blob(&mut self, _hash: &BlobHash, _payload: &[u8]) -> Result<bool, StorageError> {
        unavailable()
    }

    fn fetch_blob(&self, _hash: &BlobHash, _out: &mut Vec<u8>) -> Result<(), StorageError> {
        unavailable()
    }

    fn sync_dirs(&mut self) -> Result<(), StorageError> {
        unavailable()
    }

    fn lock(&self, _mode: StoreLockMode) -> Result<Self::LockGuard, StorageError> {
        unavailable()
    }
}

impl SekaiStore for UnavailableStore {
    type Meta = UnavailableMeta;
    type Blobs = UnavailableBlobs;
    type LockGuard = ();

    fn meta(&self) -> &Self::Meta {
        &self.meta
    }

    fn meta_mut(&mut self) -> &mut Self::Meta {
        &mut self.meta
    }

    fn cas(&self) -> &Self::Blobs {
        &self.blobs
    }

    fn cas_mut(&mut self) -> &mut Self::Blobs {
        &mut self.blobs
    }

    fn cas_and_meta(&mut self) -> (&mut Self::Blobs, &Self::Meta) {
        (&mut self.blobs, &self.meta)
    }

    fn lock(&self, _mode: StoreLockMode) -> Result<Self::LockGuard, StorageError> {
        unavailable()
    }
}

impl SekaiStoreOpener for UnavailableStore {
    fn open(url: &str) -> impl Future<Output = Result<Self, StorageError>> + Send {
        let url = url.to_owned();
        core::future::ready(Err(StorageError::UnsupportedBackend { url }))
    }

    fn init(url: &str) -> impl Future<Output = Result<Self, StorageError>> + Send {
        let url = url.to_owned();
        core::future::ready(Err(StorageError::UnsupportedBackend { url }))
    }
}
