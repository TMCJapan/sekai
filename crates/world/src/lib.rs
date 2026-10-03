//! Filesystem owner: world discovery, fingerprint observation, read-only
//! scans, and atomic file swaps.

mod discover;
mod error;
mod fingerprint;
mod observation;
mod scan;
mod swap;
mod tree;

pub use discover::{LayoutFlavor, RegionRef, sibling_path};
pub use error::WorldError;
pub use fingerprint::{file_mtime_ms, fingerprint_file};
pub use scan::{RegionScanEntry, ScanReport, ScanSkip, ScanTimings, scan_world};
pub use swap::{atomic_swap, open_image};
pub use tree::{HostWorktree, WorldTree};
