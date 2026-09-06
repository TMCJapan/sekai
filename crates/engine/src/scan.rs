//! Read-only world scan for inspection.
//!
//! This module exposes per-file size, modification time, header hash, and
//! chunk counts without touching backup semantics: it never writes, never
//! hashes payloads, and surfaces `mtime` as `None` when the platform or
//! filesystem cannot provide one (`std::fs::Metadata::modified` covers
//! Linux/Windows/macOS).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use sekai_core::{Dimension, RegionKind, RegionReader as _};

use crate::discover::discover;
use crate::error::EngineError;

/// First bytes of a region file covered by the header hash.
///
/// Region headers occupy two 4 KiB sectors (location table + timestamps);
/// hashing only the location table (first sector) is enough to detect
/// chunk add/remove/relocate, while staying cheap (one 4 KiB read worth of
/// digest input when the file is large). Files smaller than this hash
/// whatever bytes exist (including the zero-length placeholder case, which
/// hashes as empty).
pub const HEADER_HASH_LEN: usize = 4096;

/// One region file observed on disk.
#[derive(Debug, Clone)]
pub struct RegionScanEntry {
    /// Full file path.
    pub path: PathBuf,
    /// Dimension namespace.
    pub dim: Dimension,
    /// Region family.
    pub kind: RegionKind,
    /// Region X from the file name.
    pub region_x: i32,
    /// Region Z from the file name.
    pub region_z: i32,
    /// File size in bytes.
    pub file_bytes: u64,
    /// Last modification time as Unix millis, when the platform provides it.
    pub mtime_ms: Option<u64>,
    /// Chunks present in the file.
    pub chunks: usize,
    /// Lowercase hex blake3 of the first [`HEADER_HASH_LEN`] file bytes.
    pub header_hash: String,
}

/// Failure to read the platform modification time.
///
/// `metadata.modified()` fails on filesystems without timestamp support;
/// callers treat that as "unknown" rather than an error, so this helper
/// converts to `Option` at the boundary.
fn mtime_ms_of(path: &Path) -> Option<u64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let elapsed = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}

/// Scan every region file under `world` without writing anything.
pub fn scan_world(world: &Path) -> Result<Vec<RegionScanEntry>, EngineError> {
    let mut out = Vec::new();
    for region in discover(world)? {
        let bytes = fs::read(&region.path).map_err(|source| EngineError::Io {
            path: region.path.clone(),
            source,
        })?;
        let file_bytes = bytes.len() as u64;
        let header_len = bytes.len().min(HEADER_HASH_LEN);
        let header_hash = blake3::hash(&bytes[..header_len]).to_hex().to_string();
        let mtime_ms = mtime_ms_of(&region.path);
        let chunks = count_chunks(
            &bytes,
            region.dim,
            region.kind,
            region.region_x,
            region.region_z,
        )?;
        out.push(RegionScanEntry {
            path: region.path,
            dim: region.dim,
            kind: region.kind,
            region_x: region.region_x,
            region_z: region.region_z,
            file_bytes,
            mtime_ms,
            chunks,
            header_hash,
        });
    }
    Ok(out)
}

/// Count chunks in an in-memory region image.
///
/// Zero-length placeholders carry no chunks; anything else goes through the
/// strict reader so a damaged file surfaces here instead of silently
/// reporting zero.
fn count_chunks(
    bytes: &[u8],
    dim: Dimension,
    kind: RegionKind,
    region_x: i32,
    region_z: i32,
) -> Result<usize, EngineError> {
    if bytes.is_empty() {
        return Ok(0);
    }
    let file = sekai_mca::RegionFile::from_bytes(bytes.to_vec(), dim, kind, region_x, region_z)
        .map_err(EngineError::Mca)?;
    // Only the count matters here; coordinates are already validated.
    let mut chunks = 0usize;
    file.visit_chunks(|_| {
        chunks += 1;
        true
    })
    .map_err(EngineError::Mca)?;
    Ok(chunks)
}
