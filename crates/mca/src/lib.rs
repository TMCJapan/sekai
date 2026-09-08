//! Region (`.mca`) file reading and atomic rewriting.
//!
//! Rationale: the MCA sector layout (8 KiB header, 4 KiB sectors, `length +
//! type + body` payloads) is the only format knowledge in the workspace
//! besides NBT. This crate owns both directions - parsing into
//! [`sekai_core::RawChunk`] views and rebuilding files from exact CAS bytes -
//! plus world-layout discovery (finding every `.mca` and naming its
//! namespace), cheap file fingerprints for incremental snapshots, and
//! read-only inspection scans. Live files are never mutated in place:
//! writers always swap via same-directory temp file + `rename`.

mod discover;
mod error;
mod fingerprint;
mod reader;
mod region;
mod scan;
mod writer;

pub use discover::{LayoutFlavor, RegionRef, derive_path, detect_flavor, discover};
pub use error::McaError;
pub use fingerprint::{HEADER_HASH_LEN, file_mtime_ms, fingerprint_file};
pub use reader::RegionFile;
pub use region::parse_region_name;
pub use scan::{RegionScanEntry, scan_world};
pub use writer::RegionFileWriter;
