//! Region chunk and NBT normalization ports.
//!
//! Rationale: `core` owns shapes and ordering guarantees while `mca` and
//! `nbt` own I/O, codecs, and sector math. Traits use static dispatch
//! (generics) and visitor callbacks (`FnMut(...) -> bool`) so hot paths
//! avoid allocation and `dyn` overhead. Crash-consistency ordering (write
//! path: blobs before DB; GC delete path: DB before blobs) is the use-case
//! layer's responsibility and is documented per trait.

use crate::domain::coords::ChunkCoord;
use crate::domain::hash::DiffHash;

/// Byte view of one chunk as stored in a region file sector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawChunk<'a> {
    /// Global chunk identity.
    pub coord: ChunkCoord,
    /// Exact stored payload: compression-type byte + compressed data.
    ///
    /// Preserved verbatim into CAS so rollback reproduces the exact raw
    /// payload captured for the history entry (volatile tags included,
    /// as of capture time).
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
    /// skipped (absence becomes a tombstone at the use-case layer, not here).
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
