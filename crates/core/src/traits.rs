//! Boundary traits implemented by outer crates.
//!
//! Rationale: `core` owns shapes and ordering guarantees while `mca`,
//! `nbt`, and `storage` own I/O, codecs, and SQLite. Traits use static
//! dispatch (generics) and visitor callbacks (`FnMut(...) -> bool`) so hot
//! paths avoid allocation and `dyn` overhead. Crash-consistency ordering
//! (write path: blobs before DB; GC delete path: DB before blobs) is the
//! caller's (`engine`) responsibility and is documented per trait.

use crate::coords::ChunkCoord;
use crate::hash::{BlobHash, DiffHash};
use crate::history::ChunkHistoryEntry;
use crate::snapshot::{Snapshot, SnapshotId};

/// Streaming Blake3 over exact raw chunk bytes (persistent CAS key).
///
/// Implemented outside `core` (e.g. with the `blake3` crate) to keep
/// `core` dependency-free. Feed the raw payload exactly as read from the
/// `.mca` sector, including its compression framing.
pub trait BlobHasher {
    /// Fresh hashing state.
    fn new() -> Self;
    /// Feed raw payload bytes; call any number of times.
    fn update(&mut self, data: &[u8]);
    /// Finalize into the persistent CAS key.
    fn finalize(self) -> BlobHash;
}

/// Streaming Blake3 over normalized NBT (volatile diff view).
///
/// A separate type from [`BlobHasher`] so future versions may swap the
/// diff function (e.g. to a faster non-cryptographic hash) without
/// touching restore-critical CAS keys.
pub trait DiffHasher {
    /// Fresh hashing state.
    fn new() -> Self;
    /// Feed normalized bytes.
    fn update(&mut self, data: &[u8]);
    /// Finalize into the volatile view hash.
    fn finalize(self) -> DiffHash;
}

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
    /// Implementations must write atomically (temp file + rename) and
    /// fsync before returning, so a later DB commit never dangles.
    fn put(&mut self, hash: &BlobHash, payload: &[u8]) -> Result<bool, Self::Error>;

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

/// MVCC metadata over snapshots and per-chunk history (by `storage`).
pub trait MetaStore {
    /// Backend failure (SQLite errors, constraint violations, ...).
    type Error;

    /// Append a snapshot; IDs are monotone increasing.
    fn create_snapshot(&mut self, created_at_ms: u64) -> Result<SnapshotId, Self::Error>;

    /// Record one chunk's state at `snapshot`. `blob = None` is a tombstone.
    fn record_chunk(
        &mut self,
        snapshot: SnapshotId,
        coord: &ChunkCoord,
        blob: Option<&BlobHash>,
        diff: Option<&DiffHash>,
    ) -> Result<(), Self::Error>;

    /// Exact row for (`snapshot`, `coord`), or `None` when never recorded.
    fn lookup_chunk(
        &self,
        snapshot: SnapshotId,
        coord: &ChunkCoord,
    ) -> Result<Option<ChunkHistoryEntry>, Self::Error>;

    /// Snapshot metadata for `id`, or `None` when it does not exist.
    ///
    /// Point query behind the seam so callers never scan the full timeline
    /// to resolve one snapshot.
    fn lookup_snapshot(&self, id: SnapshotId) -> Result<Option<Snapshot>, Self::Error>;

    /// Highest-ID snapshot, or `None` when no backup has run yet.
    fn latest_snapshot(&self) -> Result<Option<Snapshot>, Self::Error>;

    /// Visit every row of one snapshot. Return `false` to stop early.
    ///
    /// Visitor style keeps full-snapshot rollback streaming without
    /// materializing all rows (1024+ per region file) in memory.
    fn visit_snapshot_chunks<F>(&self, snapshot: SnapshotId, visit: F) -> Result<(), Self::Error>
    where
        F: FnMut(&ChunkHistoryEntry) -> bool;

    /// Visit snapshots in ID order. Return `false` to stop early.
    fn visit_snapshots<F>(&self, visit: F) -> Result<(), Self::Error>
    where
        F: FnMut(&Snapshot) -> bool;
}

/// Byte view of one chunk as stored in a region file sector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawChunk<'a> {
    /// Global chunk identity.
    pub coord: ChunkCoord,
    /// Exact stored payload: compression-type byte + compressed data.
    ///
    /// Preserved verbatim into CAS so rollback is byte-perfect.
    pub payload: &'a [u8],
}

impl<'a> RawChunk<'a> {
    /// Construct a raw chunk view.
    #[inline]
    #[must_use]
    pub const fn new(coord: ChunkCoord, payload: &'a [u8]) -> Self {
        Self { coord, payload }
    }
}

/// Read side of `.mca` files (implemented by `mca`).
pub trait RegionReader {
    /// Decode failure (truncated file, bad sector table, ...).
    type Error;

    /// Visit every present chunk in one region file. Absent sectors are
    /// skipped (absence becomes a tombstone at the `engine` layer, not here).
    /// Return `false` to stop early.
    fn visit_chunks<F>(&self, visit: F) -> Result<(), Self::Error>
    where
        F: FnMut(RawChunk<'_>) -> bool;
}

/// Write side of `.mca` files (implemented by `mca`).
///
/// Implementations must buffer into a temp file in the *same directory* as
/// the target (cross-mount `rename` safety) and only swap on [`commit`](RegionWriter::commit).
pub trait RegionWriter {
    /// Encode/I/O failure.
    type Error;

    /// Stage a chunk payload (exact bytes from CAS).
    fn stage_chunk(&mut self, coord: &ChunkCoord, payload: &[u8]) -> Result<(), Self::Error>;

    /// Stage removal of a chunk (rollback onto a tombstone).
    fn stage_remove(&mut self, coord: &ChunkCoord) -> Result<(), Self::Error>;

    /// Atomically replace the target file with staged contents.
    fn commit(self) -> Result<(), Self::Error>;
}

/// NBT normalization for volatile diff views (implemented by `nbt`).
///
/// Input is the raw stored payload; decompression and tag zeroing (e.g.
/// `LastUpdate`) happen inside the implementation. Stored blobs are never
/// modified - the result feeds [`DiffHash`] only.
pub trait Normalizer {
    /// Decode/normalization failure (corrupt NBT, unknown compression, ...).
    type Error;

    /// Compute the volatile diff hash for one raw payload.
    fn diff_hash(&self, raw_payload: &[u8]) -> Result<DiffHash, Self::Error>;
}
