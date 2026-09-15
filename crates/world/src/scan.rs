//! Read-only world scan for inspection.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sekai_core::{Dimension, RegionKind};

use crate::discover::{RegionRef, discover};
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

/// Per-phase timings for [`scan_world`]. Informational only; never
/// changes scan semantics.
#[derive(Debug, Clone, Default)]
pub struct ScanTimings {
    /// Wall-clock total.
    pub total: Duration,
    /// Region file discovery.
    pub discover: Duration,
    /// File reads (plus mtime observation).
    pub read: Duration,
    /// Header hashing and chunk counting.
    pub parse: Duration,
}

/// Scan every region file under `world` without writing anything.
///
/// A single corrupt or unreadable region does not abort the whole scan;
/// that entry is skipped so healthy regions remain inspectable.
pub fn scan_world(world: &Path) -> Result<(Vec<RegionScanEntry>, ScanTimings), WorldError> {
    let total_started = Instant::now();
    let discover_started = Instant::now();
    let regions = discover(world)?;
    let discover = discover_started.elapsed();
    let mut read = Duration::ZERO;
    let mut parse = Duration::ZERO;
    let mut out = Vec::new();
    for region in regions {
        let read_started = Instant::now();
        let outcome = read_with_mtime(&region.path);
        read += read_started.elapsed();
        let Ok((bytes, mtime_ms)) = outcome else {
            continue;
        };
        let parse_started = Instant::now();
        let entry = parse_entry(&region, &bytes, mtime_ms);
        parse += parse_started.elapsed();
        if let Some(entry) = entry {
            out.push(entry);
        }
    }
    let timings = ScanTimings {
        total: total_started.elapsed(),
        discover,
        read,
        parse,
    };
    Ok((out, timings))
}

/// Hash one file's header and count its chunks; `None` skips corrupt files.
fn parse_entry(region: &RegionRef, bytes: &[u8], mtime_ms: Option<u64>) -> Option<RegionScanEntry> {
    let file_bytes = bytes.len() as u64;
    let header_len = bytes.len().min(HEADER_HASH_LEN);
    let header_hash = hex(&sekai_anvil::header_hash(&bytes[..header_len]));
    let chunks = count_chunks(bytes, region.region_x, region.region_z).ok()?;
    Some(RegionScanEntry {
        path: region.path.clone(),
        dim: region.dim,
        kind: region.kind,
        region_x: region.region_x,
        region_z: region.region_z,
        file_bytes,
        mtime_ms,
        chunks,
        header_hash,
    })
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
