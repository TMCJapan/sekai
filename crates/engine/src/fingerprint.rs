//! Cheap file fingerprints for incremental snapshots.
//!
//! Rationale: deciding "did this region change?" must not cost a full file
//! read plus per-chunk hashing, or the check saves nothing. The fingerprint
//! combines `mtime` and `size` from the open handle with a blake3 over the
//! location-table sector (the first 4 KiB, read only). All three must match
//! stored state to skip ingestion; the location table is covered (rather
//! than the timestamps sector) so timestamp-only rewrites still skip while
//! any chunk add/remove/relocate forces ingest.

use std::fs::File;
use std::io::Read as _;
use std::path::Path;
use std::time::UNIX_EPOCH;

use crate::error::EngineError;

/// First bytes of a region file covered by the header hash.
///
/// Region headers occupy two 4 KiB sectors (location table + timestamps);
/// only the location table feeds the fingerprint, so timestamp-only
/// rewrites keep matching while chunk add/remove/relocate always mismatches.
pub const HEADER_HASH_LEN: usize = 4096;

/// Cheap identity of one region file on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileFingerprint {
    /// File size in bytes.
    pub size: u64,
    /// Last modification time as Unix millis, when the platform provides it.
    ///
    /// Read through `std::fs::Metadata::modified`, the portable seam over
    /// Linux, Windows, and macOS. `None` forces ingest downstream.
    pub mtime_ms: Option<u64>,
    /// Blake3 of the first [`HEADER_HASH_LEN`] file bytes.
    pub header_hash: [u8; 32],
}

/// Last modification time as Unix millis, or `None` when unavailable.
///
/// Filesystems without timestamp support surface an error here; callers
/// treat that as "unknown" rather than a failure.
#[must_use]
pub fn file_mtime_ms(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let elapsed = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}

/// Fingerprint one region file without reading it fully.
///
/// Opens the file first and derives `size`/`mtime` from the open handle so
/// all three signals describe the same inode; a file replaced between
/// `stat` and `open` would otherwise mix metadata from one version with
/// header bytes from another. Only `fstat` on the open handle plus the first
/// [`HEADER_HASH_LEN`] bytes are touched; short files (including
/// zero-length placeholders) hash whatever bytes exist.
pub fn fingerprint_file(path: &Path) -> Result<FileFingerprint, EngineError> {
    let io = |source: std::io::Error| EngineError::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut file = File::open(path).map_err(&io)?;
    let meta = file.metadata().map_err(&io)?;
    let size = meta.len();
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let mut header = [0u8; HEADER_HASH_LEN];
    let mut read = 0usize;
    while read < HEADER_HASH_LEN {
        match file.read(&mut header[read..]) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(source) if source.kind() == std::io::ErrorKind::Interrupted => {}
            Err(source) => return Err(io(source)),
        }
    }
    let digest = blake3::hash(&header[..read]);
    Ok(FileFingerprint {
        size,
        mtime_ms,
        header_hash: *digest.as_bytes(),
    })
}
