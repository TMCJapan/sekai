//! Rebuildable region-image cache boundary.
//!
//! The cache maps a region node hash (the `region_tree` primary key) to the
//! undecomposed `.mca` image that produced it and that image's content hash
//! (`hash_bytes` over the whole file). It is derived state: every entry can
//! be recomputed from the world and the snapshot tree, so an absent entry is
//! a cold cache, never corruption, and wiping the cache costs performance
//! only.
//!
//! The payoff is the file-level change check ADR-0023 left open: a caller
//! that knows a commit's region hash reads the cached image hash without
//! touching the image bytes, hashes the live file, and compares - proving a
//! fully covered region unchanged without decompressing or normalizing a
//! single chunk. A mismatch or a miss falls back to the full per-chunk path;
//! the cache never records absence and never decides a commit on its own.
//!
//! Backends that perform blocking work (filesystem or database calls) must
//! run it on the runtime's blocking pool at the app layer, never inline on
//! an async worker.

use alloc::vec::Vec;
use core::future::Future;
use sekai_util::RegionHash;

/// Rebuildable cache of undecomposed region images, keyed by region node hash.
pub trait CacheStore {
    /// Backend error type.
    type Error;

    /// Store `image` and its content hash under the region node hash.
    /// Returns `true` when newly inserted.
    ///
    /// `mca_hash` must be the `hash_bytes` digest of `image`; the backend
    /// stores the pair as given and never re-hashes. The image and its hash
    /// must become visible together: a reader that sees one sees the other.
    ///
    /// Storing an existing region hash is an idempotent `false`: the first
    /// image wins. Different raw files can normalize to the same region
    /// node, so any image that produced the hash is a valid representative.
    fn put(
        &mut self,
        region: RegionHash,
        mca_hash: [u8; 32],
        image: &[u8],
    ) -> impl Future<Output = Result<bool, Self::Error>> + Send;

    /// The cached image's content hash, without reading the image bytes.
    ///
    /// `None` when the region has no entry: a cold cache is normal, never
    /// an error. This is the cheap side of the file-level change check.
    fn mca_hash(
        &self,
        region: RegionHash,
    ) -> impl Future<Output = Result<Option<[u8; 32]>, Self::Error>> + Send;

    /// Load the cached image into `out`, clearing it first. Returns `false`
    /// when the region has no entry, leaving `out` empty.
    fn fetch_image(
        &self,
        region: RegionHash,
        out: &mut Vec<u8>,
    ) -> impl Future<Output = Result<bool, Self::Error>> + Send;

    /// Flush cached entries to stable storage.
    ///
    /// Advisory, not a crash-order barrier: the cache is derived, so a torn
    /// write leaves at most a missing entry. Callers may batch it like the
    /// blob barrier to keep the log small.
    fn sync(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Remove every cached entry, returning how many were removed.
    ///
    /// Always safe: the cache is derived state, so wiping it costs
    /// performance only and never changes what a commit or checkout reads
    /// or writes. The keyspace and its format marker survive; the next
    /// commit repopulates the entries it needs. Entries written concurrently
    /// with the clear may survive; they are equally derived and harmless.
    fn clear(&mut self) -> impl Future<Output = Result<u64, Self::Error>> + Send;
}
