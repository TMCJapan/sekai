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

    /// Read the whole file.
    fn read(&self) -> Result<Vec<u8>, Self::Error>;

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
    /// A file that is already gone fails like any other failure. The
    /// error type is the tree's own, so a caller that must tell "already
    /// gone" from a real I/O error downcasts it.
    fn remove(&self) -> Result<(), Self::Error>;
}
