//! Content-addressed blob storage port (implemented by `storage`).
//!
//! Ordering contract: callers must flush + fsync blobs *before* recording
//! history rows that reference them.

use crate::domain::hash::BlobHash;

/// Content-addressed blob storage (implemented by `storage`).
///
/// Ordering contract: callers must flush + fsync blobs *before* recording
/// history rows that reference them.
pub trait BlobStore {
    /// Backend failure (I/O, permissions, ...).
    type Error;

    /// Whether `hash` is already stored (for dedup accounting).
    fn contains(&self, hash: &BlobHash) -> Result<bool, Self::Error>;

    /// Store `payload` under `hash`. Returns `true` when newly inserted.
    ///
    /// Implementations must write atomically (temp file + rename) and fsync
    /// file data before returning. Directory durability may be deferred to
    /// [`BlobStore::sync`]; callers must invoke it before committing any
    /// metadata that references the new blobs.
    fn put(&mut self, hash: &BlobHash, payload: &[u8]) -> Result<bool, Self::Error>;

    /// Make all stored blobs crash-durable (barrier before metadata commit).
    ///
    /// Persists whatever `put` deferred (e.g. directory entries) in bulk, so
    /// one call covers a whole batch instead of one fsync per blob. A later
    /// DB commit referencing the blobs must never dangle after this returns.
    fn sync(&mut self) -> Result<(), Self::Error>;

    /// Load the blob into `out`, clearing it first.
    ///
    /// Errors when the blob is missing; callers treat a missing blob as
    /// database corruption, never as a tombstone (tombstones are `None`
    /// history rows, not absent files).
    fn fetch_into(&self, hash: &BlobHash, out: &mut alloc::vec::Vec<u8>)
    -> Result<(), Self::Error>;

    /// Remove `hash` from the store; returns `false` when already absent.
    ///
    /// Ordering contract: callers must settle metadata *before* unlinking
    /// blobs, so a crash never leaves history pointing at nothing. GC
    /// re-verifies unreachability just before removing, so a blob
    /// referenced after planning is never deleted.
    fn remove(&mut self, hash: &BlobHash) -> Result<bool, Self::Error>;

    /// Visit every stored blob hash. Return `false` to stop early.
    ///
    /// Names that do not decode as blob hashes (temp leftovers, foreign
    /// files) are skipped: never visited and never removed by GC.
    fn visit_blobs<F>(&self, visit: F) -> Result<(), Self::Error>
    where
        F: FnMut(&BlobHash) -> bool;
}
