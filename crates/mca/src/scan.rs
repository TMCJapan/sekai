//! Read-only world scan for inspection.
//!
//! This module exposes per-file size, modification time, header hash, and
//! chunk counts without touching backup semantics: it never writes, never
//! hashes payloads, and surfaces `mtime` as `None` when the platform or
//! filesystem cannot provide one (`std::fs::Metadata::modified` covers
//! Linux/Windows/macOS).

use std::fs;
use std::path::{Path, PathBuf};

use sekai_core::{Dimension, RegionKind, RegionReader as _};

use crate::discover::discover;
use crate::error::McaError;
use crate::fingerprint::{HEADER_HASH_LEN, file_mtime_ms};

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

/// Scan every region file under `world` without writing anything.
pub fn scan_world(world: &Path) -> Result<Vec<RegionScanEntry>, McaError> {
    let mut out = Vec::new();
    for region in discover(world)? {
        let bytes = fs::read(&region.path).map_err(|source| McaError::Io {
            path: region.path.clone(),
            source,
        })?;
        let file_bytes = bytes.len() as u64;
        let header_len = bytes.len().min(HEADER_HASH_LEN);
        let header_hash = blake3::hash(&bytes[..header_len]).to_hex().to_string();
        let mtime_ms = file_mtime_ms(&region.path);
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
) -> Result<usize, McaError> {
    if bytes.is_empty() {
        return Ok(0);
    }
    let file = crate::RegionFile::from_bytes(bytes.to_vec(), dim, kind, region_x, region_z)?;
    // Only the count matters here; coordinates are already validated.
    let mut chunks = 0usize;
    file.visit_chunks(|_| {
        chunks += 1;
        true
    })?;
    Ok(chunks)
}
