//! Content-addressed blob storage boundary.
//!
//! Blobs must be durable before metadata references them.
//!
//! File-backed implementations perform short blocking filesystem calls
//! inline; hot paths must run them under the runtime's blocking pool at
//! the app layer.
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
    /// Implementations must write atomically (temp file + rename) and sync
    /// file data before returning. Directory durability may be deferred to
    /// [`sync`](BlobStore::sync); callers must invoke it before committing
    /// any metadata that references the new blobs.
    fn put(
        &mut self,
        hash: &BlobHash,
        payload: &[u8],
    ) -> impl Future<Output = Result<bool, Self::Error>> + Send;

    /// Make stored blobs crash-durable before metadata commit.
    fn sync(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Load the blob into `out`, clearing it first.
    ///
    /// Errors when the blob is missing; callers treat a missing blob as
    /// database corruption, never as a tombstone (tombstones are `None`
    /// history rows, not absent files).
    fn fetch_into(
        &self,
        hash: &BlobHash,
        out: &mut Vec<u8>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Remove a blob; return `false` when it is already absent.
    fn remove(&mut self, hash: &BlobHash)
    -> impl Future<Output = Result<bool, Self::Error>> + Send;

    /// Visit every stored blob hash. Return `false` to stop early.
    ///
    /// Names that do not decode as blob hashes (temp leftovers, foreign
    /// files) are skipped: never visited and never removed by GC.
    fn visit_blobs<F>(&self, visit: F) -> impl Future<Output = Result<(), Self::Error>> + Send
    where
        F: FnMut(&BlobHash) -> bool + Send;
}
