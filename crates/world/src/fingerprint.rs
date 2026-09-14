//! Fingerprinting region files for incremental snapshots.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use sekai_core::{RegionFingerprint, RegionKey};

use crate::error::WorldError;

/// First bytes of a region file covered by the header hash.
pub const HEADER_HASH_LEN: usize = 4096;

pub(crate) fn mtime_ms_from_system_time(t: SystemTime) -> Option<u64> {
    let elapsed = t.duration_since(UNIX_EPOCH).ok()?;
    Some(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}

pub(crate) fn mtime_ms_from_metadata(meta: &std::fs::Metadata) -> Option<u64> {
    mtime_ms_from_system_time(meta.modified().ok()?)
}

/// Last modification time as Unix millis, or `None` when unavailable.
pub fn file_mtime_ms(path: &Path) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    mtime_ms_from_metadata(&meta)
}

fn read_header(file: &mut File) -> Result<Vec<u8>, std::io::Error> {
    let mut buf = Vec::with_capacity(HEADER_HASH_LEN);
    file.take(HEADER_HASH_LEN as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

/// Fingerprint one region file without reading it fully.
pub fn fingerprint_file(path: &Path, key: RegionKey) -> Result<RegionFingerprint, WorldError> {
    let mut file = File::open(path).map_err(|e| WorldError::io(path, e))?;
    let meta = file.metadata().map_err(|e| WorldError::io(path, e))?;
    let size = meta.len();
    let mtime_ms = mtime_ms_from_metadata(&meta);
    let header = read_header(&mut file).map_err(|e| WorldError::io(path, e))?;

    Ok(RegionFingerprint {
        key,
        size,
        mtime_ms,
        header_hash: sekai_anvil::header_hash(&header),
    })
}
