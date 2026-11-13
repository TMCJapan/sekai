//! Content-addressed blob storage boundary.
//!
//! Blobs must be durable before metadata references them. The store is
//! append-only by nature: keys are content hashes and a stored blob never
//! changes or disappears, so concurrent readers and writers need no lock.
//!
//! Backends that perform blocking work (filesystem or database calls) must
//! run it on the runtime's blocking pool at the app layer, never inline on
//! an async worker.
use alloc::vec::Vec;
use core::future::Future;
use sekai_util::ChunkHash;

/// Content-addressed blob storage.
pub trait BlobStore {
    /// Backend error type.
    type Error;

    /// Check whether a blob already exists.
    fn contains(&self, hash: &ChunkHash) -> impl Future<Output = Result<bool, Self::Error>> + Send;

    /// Store `payload` under `hash`. Returns `true` when newly inserted.
    ///
    /// Each write must be atomic: a reader sees the whole payload or no
    /// blob. The write must be durable before any metadata that references
    /// it commits. A backend may batch that barrier into
    /// [`sync`](BlobStore::sync); callers must invoke it before committing
    /// any metadata that references the new blobs.
    ///
    /// Storing an existing blob is an idempotent, deduplicated `false`:
    /// never an error and never a rewrite. Concurrent puts of the same hash
    /// are expected and never need coordination.
    fn put(
        &mut self,
        hash: &ChunkHash,
        payload: &[u8],
    ) -> impl Future<Output = Result<bool, Self::Error>> + Send;

    /// Make stored blobs crash-durable before metadata commit.
    ///
    /// Backends that already sync every [`put`](BlobStore::put) may treat
    /// this as a no-op; batching backends use it as their write barrier.
    fn sync(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Load the blob into `out`, clearing it first.
    ///
    /// Errors when the blob is missing; callers treat a missing blob as
    /// database corruption, never as a tombstone (tombstones are `None`
    /// history rows, not absent blobs).
    fn fetch_into(
        &self,
        hash: &ChunkHash,
        out: &mut Vec<u8>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Visit every stored blob hash in an implementation-defined order.
    /// Return `false` to stop early.
    fn visit_blobs<F>(&self, visit: F) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        F: FnMut(&ChunkHash) -> bool + Send;
}
