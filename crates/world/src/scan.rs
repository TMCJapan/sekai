//! Read-only world scan for inspection.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use sekai_core::{Dimension, RegionKind};

use crate::discover::discover;
use crate::error::WorldError;
use crate::fingerprint::HEADER_HASH_LEN;

/// One region file observed on disk.
#[derive(Debug, Clone)]
pub struct RegionScanEntry {
    pub path: PathBuf,
    pub dim: Dimension,
    pub kind: RegionKind,
    pub region_x: i32,
    pub region_z: i32,
    pub file_bytes: u64,
    pub mtime_ms: Option<u64>,
    pub chunks: usize,
    pub header_hash: String,
}

/// Scan every region file under `world` without writing anything.
///
/// A single corrupt or unreadable region does not abort the whole scan;
/// that entry is skipped so healthy regions remain inspectable.
pub fn scan_world(world: &Path) -> Result<Vec<RegionScanEntry>, WorldError> {
    let mut out = Vec::new();
    for region in discover(world)? {
        let Ok((bytes, mtime_ms)) = read_with_mtime(&region.path) else {
            continue;
        };
        let file_bytes = bytes.len() as u64;
        let header_len = bytes.len().min(HEADER_HASH_LEN);
        let header_hash = hex(&sekai_anvil::header_hash(&bytes[..header_len]));
        let Ok(chunks) = count_chunks(&bytes, region.region_x, region.region_z) else {
            continue;
        };
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

fn read_with_mtime(path: &Path) -> Result<(Vec<u8>, Option<u64>), WorldError> {
    let mut file = File::open(path).map_err(|e| WorldError::io(path, e))?;
    let meta = file.metadata().ok();
    let mtime_ms = meta
        .as_ref()
        .and_then(crate::fingerprint::mtime_ms_from_metadata);
    let mut bytes = Vec::with_capacity(
        meta.as_ref()
            .map_or(0, |m| usize::try_from(m.len()).unwrap_or(0)),
    );
    file.read_to_end(&mut bytes)
        .map_err(|e| WorldError::io(path, e))?;
    Ok((bytes, mtime_ms))
}

fn hex(bytes: &[u8; 32]) -> String {
    use core::fmt::Write;
    let mut out = String::with_capacity(64);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn count_chunks(bytes: &[u8], region_x: i32, region_z: i32) -> Result<usize, WorldError> {
    if bytes.is_empty() {
        return Ok(0);
    }
    let file = sekai_anvil::RegionImage::from_bytes(bytes.to_vec(), region_x, region_z)?;
    let mut chunks = 0usize;
    file.visit_chunks(|_| {
        chunks += 1;
        true
    })?;
    Ok(chunks)
}
