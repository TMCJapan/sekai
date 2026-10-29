//! Region file handles: the read/write capability the world operations use.

use sekai_core::{RegionFingerprint, RegionKey};

/// One region file, as the only capability the composition layer gets.
///
/// The handle reads the file, observes its fingerprint, writes a rebuilt
/// image, and removes the file - nothing else. Where the file lives and
/// which file counts as its sibling stay with the
/// [`WorldTree`](crate::WorldTree) that produced the handle, so a tree can
/// be backed by memory instead of a filesystem.
///
/// The handle is also [`std::fmt::Display`]: the text names the file for
/// diagnostics (error messages, timing rows). A tree that has no location of
/// its own spells it as it sees fit.
pub trait RegionFile: Clone + std::fmt::Debug + std::fmt::Display + Send + Sync + 'static {
    /// Failure type for every operation on this file.
    type Error: std::error::Error;

    /// The parsed image [`RegionFile::read`] returns.
    type RegionImage: Region<Error = Self::Error>;

    /// Read the whole file and parse it into an image.
    ///
    /// `key` names the region the file holds, like [`RegionFile::fingerprint`].
    fn read(&self, key: RegionKey) -> Result<Self::RegionImage, Self::Error>;

    /// Observe the file's fingerprint as of now.
    ///
    /// `key` names the region the file holds: a file carries no identity of
    /// its own, so the caller supplies the one it discovered.
    fn fingerprint(&self, key: RegionKey) -> Result<RegionFingerprint, Self::Error>;

    /// Whether the file exists.
    ///
    /// A file that cannot be inspected (an unreadable directory, a broken
    /// symlink) counts as absent: the question is "is there something to
    /// read", not "can this be read".
    fn exists(&self) -> bool;

    /// Replace the file's content with `image`.
    fn write(&self, image: &[u8]) -> Result<(), Self::Error>;

    /// Delete the file.
    ///
    /// A file that is already gone fails like any other failure. Telling
    /// that case apart from a real I/O error takes the tree's concrete
    /// error type: the trait promises no distinguishable failure.
    fn remove(&self) -> Result<(), Self::Error>;
}

/// A region file parsed in memory.
///
/// Callers ask it questions in global chunk coordinates and never rebuild a
/// parser image from raw bytes themselves: the handle that read the file
/// owns the conversion and attaches its location to parse failures.
///
/// The signatures mirror [`sekai_anvil::RegionImage`]. The error type is not
/// tied to the backing parser, so a tree that is not the host tree (an
/// in-memory fake, another backend) can serve images of its own.
pub trait Region {
    /// Failure type for every query on this image.
    type Error;

    /// Visits all present chunks in table order.
    ///
    /// Returning `false` stops iteration.
    fn visit_chunks<F>(&self, visit: F) -> Result<(), Self::Error>
    where
        F: FnMut(sekai_anvil::Chunk<'_>) -> bool;

    /// Returns the stored raw payload for a single chunk at global coordinates `(x, z)`.
    ///
    /// Returns `Ok(None)` if the chunk coordinate is outside this region or if the
    /// chunk has not been generated (is empty). Callers need no finer answer:
    /// both mean the region holds no payload for that coordinate.
    fn chunk_payload(&self, x: i32, z: i32) -> Result<Option<&[u8]>, Self::Error>;
}
