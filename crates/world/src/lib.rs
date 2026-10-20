//! World access: discovery, fingerprint observation, and atomic file
//! swaps.
//!
//! The [`WorldTree`] trait is the boundary the composition layer talks
//! to: a tree names the region files it holds and hands out
//! [`RegionFile`] handles that read, fingerprint, write, and remove them.
//! [`HostWorldTree`] is the filesystem-backed tree.

mod error;
mod fingerprint;
mod lock;
mod observation;
mod region;
mod swap;
mod tree;

pub use error::WorldError;
pub use fingerprint::{file_mtime_ms, fingerprint_file};
pub use lock::{WorldLock, WorldLockMode};
pub use region::RegionFile;
pub use swap::{atomic_swap, open_image};
pub use tree::{HostRegion, HostWorldLock, HostWorldTree, WorldTree};
