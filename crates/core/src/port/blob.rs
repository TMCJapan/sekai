//! Content-addressed blob storage boundary.
//!
//! Blobs must be durable before metadata references them.
//!
//! Backends that perform blocking work (filesystem or database calls) must
//! run it on the runtime's blocking pool at the app layer, never inline on
//! an async worker.
use alloc::vec::Vec;
use core::future::Future;
use sekai_util::BlobHash;

/// Content-addressed blob storage.
pub trait BlobStore {
    /// Backend error type.
    type Error;

    /// Check whether a blob already exists.
    fn contains(&self, hash: &BlobHash) -> impl Future<Output = Result<bool, Self::Error>> + Send;

    /// Store `payload` under `hash`. Returns `true` when newly inserted.
    ///
    /// Each write must be atomic: a reader sees the whole payload or no
    /// blob. The write must be durable before any metadata that references
    /// it commits. A backend may batch that barrier into
    /// [`sync`](BlobStore::sync); callers must invoke it before committing
    /// metadata. Storing an existing hash is a deduplicated `false`, never
    /// an error or a rewrite.
    fn put(
        &mut self,
        hash: &BlobHash,
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
        hash: &BlobHash,
        out: &mut Vec<u8>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Remove a blob; return `false` when it is already absent.
    fn remove(&mut self, hash: &BlobHash)
    -> impl Future<Output = Result<bool, Self::Error>> + Send;

    /// Visit every stored blob hash in an implementation-defined order.
    /// Return `false` to stop early.
    fn visit_blobs<F>(&self, visit: F) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        F: FnMut(&BlobHash) -> bool + Send;
}
