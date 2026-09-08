#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Region (`.mca`) file reading and atomic rewriting.
//!
//! Rationale: the MCA sector layout (8 KiB header, 4 KiB sectors, `length +
//! type + body` payloads) is the only format knowledge in the workspace
//! besides NBT. This crate owns both directions - parsing into
//! [`sekai_core::RawChunk`] views and rebuilding files from exact CAS bytes -
//! so reproducing captured payloads never depends on ad-hoc format code in
//! `engine`. Live files are never mutated in place: writers always swap via
//! same-directory temp file + `rename`.

mod error;
mod reader;
mod region;
mod writer;

pub use error::McaError;
pub use reader::RegionFile;
pub use region::parse_region_name;
pub use writer::RegionFileWriter;
