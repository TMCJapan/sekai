//! Fingerprinting region files for incremental snapshots.

use std::fs::File;
use std::path::Path;

use sekai_core::{RegionFingerprint, RegionKey};

use crate::error::WorldError;
use crate::observation::{content_hash_of, mtime_ms_from_metadata};

/// Last modification time as Unix millis, or `None` when unavailable.
pub fn file_mtime_ms(path: &Path) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    mtime_ms_from_metadata(&meta)
}

/// Fingerprint one region file.
///
/// The content hash covers every byte, streamed: a header-only hash left
/// everything past the 4 KiB location table invisible, so a chunk rewritten
/// to the same compressed length under a preserved mtime (`cp -p`,
/// `rsync -t`, `tar -x`, a ZFS/Btrfs rollback, a coarse-mtime filesystem)
/// was declared unchanged. The backup then stored nothing and a later
/// rollback overwrote bytes it had never captured. Reading the file is the
/// price of not silently skipping a real change; it replaces a 4 KiB read
/// with one sequential pass that the ingest path reads anyway.
pub fn fingerprint_file(path: &Path, key: RegionKey) -> Result<RegionFingerprint, WorldError> {
    let mut file = File::open(path).map_err(|e| WorldError::io(path, e))?;
    let meta = file.metadata().map_err(|e| WorldError::io(path, e))?;
    let size = meta.len();
    let mtime_ms = mtime_ms_from_metadata(&meta);
    let content_hash = content_hash_of(&mut file).map_err(|e| WorldError::io(path, e))?;

    Ok(RegionFingerprint {
        key,
        size,
        mtime_ms,
        content_hash,
    })
}
