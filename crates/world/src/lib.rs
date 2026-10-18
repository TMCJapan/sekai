//! Filesystem owner: world discovery, fingerprint observation, and atomic
//! file swaps.

mod discover;
mod error;
mod fingerprint;
mod lock;
mod observation;
mod swap;
mod tree;

pub use discover::{RegionRef, sibling_path};
pub use error::WorldError;
pub use fingerprint::{file_mtime_ms, fingerprint_file};
pub use lock::{WorldLock, WorldLockMode};
pub use swap::{atomic_swap, open_image};
pub use tree::{HostWorldTree, WorldTree};
