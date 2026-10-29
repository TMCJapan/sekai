//! World access: discovery, fingerprint observation, and atomic file
//! swaps.
//!
//! The [`WorldTree`] trait is the boundary the composition layer talks
//! to: a tree names the region files it holds and hands out
//! [`RegionFile`] handles that read, fingerprint, write, and remove them.
//! [`HostWorldTree`] is the filesystem-backed tree.
//!
//! [`WorldTree::lock`] excludes concurrent sekai operations over one
//! world: [`WorldLockMode`] says how the operation uses it, and the
//! [`WorldLock`] guard the tree hands out keeps the world excluded until
//! it drops. [`HostWorldLock`] is the file-lock form the host tree uses.

mod error;
mod fingerprint;
mod lock;
mod observation;
mod region;
mod swap;
mod tree;

pub use error::WorldError;
pub use fingerprint::{file_mtime_ms, fingerprint_file};
pub use lock::{UnguardedReason, WorldLock, WorldLockMode, WorldLockState};
pub use region::{Region, RegionFile};
pub use swap::{atomic_swap, open_image};
pub use tree::{HostRegion, HostWorldLock, HostWorldLockError, HostWorldTree, WorldTree};
