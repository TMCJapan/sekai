//! Fingerprinting region files for incremental snapshots.

use std::fs::File;
use std::path::Path;

use sekai_core::{RegionFingerprint, RegionKey};

use crate::error::WorldError;
use crate::observation::{content_hash_of, mtime_ms_from_metadata};

/// Last modification time as Unix millis, or `None` when unavailable.
pub fn file_mtime_ms(path: impl AsRef<Path>) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    mtime_ms_from_metadata(&meta)
}

/// Fingerprint one region file.
///
/// The content hash covers every byte, streamed, so a payload edit survives
/// even when the compressed length and the mtime both stay put - the case a
/// header-only hash missed. Reading the file is the price of that: one
/// sequential pass the ingest path pays anyway. See ADR-0012.
pub fn fingerprint_file(
    path: impl AsRef<Path>,
    key: RegionKey,
) -> Result<RegionFingerprint, WorldError> {
    let mut file = File::open(path.as_ref()).map_err(|e| WorldError::io(path.as_ref(), e))?;
    let meta = file
        .metadata()
        .map_err(|e| WorldError::io(path.as_ref(), e))?;
    let size = meta.len();
    let mtime_ms = mtime_ms_from_metadata(&meta);
    let content_hash = content_hash_of(&mut file).map_err(|e| WorldError::io(path.as_ref(), e))?;

    Ok(RegionFingerprint {
        key,
        size,
        mtime_ms,
        content_hash,
    })
}
