//! Filesystem owner: world discovery, fingerprint observation, read-only
//! scans, and atomic file swaps.

mod discover;
mod error;
mod fingerprint;
mod scan;
mod swap;

pub use discover::{LayoutFlavor, RegionRef, derive_path, detect_flavor, discover};
pub use error::WorldError;
pub use fingerprint::{HEADER_HASH_LEN, file_mtime_ms, fingerprint_file};
pub use scan::{RegionScanEntry, scan_world};
pub use swap::{atomic_swap, open_image};
