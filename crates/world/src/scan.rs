//! Read-only world scan for inspection.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sekai_core::{Dimension, RegionKind};

use crate::discover::{RegionRef, discover};
use crate::error::WorldError;
use crate::observation::{content_hash_of_bytes, hex_hash, mtime_ms_from_metadata};

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
    /// Last modification time as Unix millis (`None` when unavailable).
    pub mtime_ms: Option<u64>,
    /// Chunks present in the file.
    pub chunks: usize,
    /// Hex content hash.
    pub content_hash: String,
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
    /// Content hashing and chunk counting.
    pub parse: Duration,
}

/// A region file the scan could not inspect, with the reason.
#[derive(Debug, Clone)]
pub struct ScanSkip {
    /// Full file path.
    pub path: PathBuf,
    /// Why the file was skipped.
    pub reason: String,
}

/// Result of a read-only world scan: what was inspected, what was not.
#[derive(Debug, Clone)]
pub struct ScanReport {
    /// Successfully inspected region files.
    pub entries: Vec<RegionScanEntry>,
    /// Files that could not be read or parsed.
    pub skipped: Vec<ScanSkip>,
    /// Per-phase timings.
    pub timings: ScanTimings,
}

/// Scan every region file under `world` without writing anything.
///
/// A single corrupt or unreadable region does not abort the whole scan;
/// it is reported in [`ScanReport::skipped`] instead of vanishing, so an
/// inventory never silently undercounts the files on disk.
pub fn scan_world(world: &Path) -> Result<ScanReport, WorldError> {
    let total_started = Instant::now();
    let discover_started = Instant::now();
    let regions = discover(world)?;
    let discover = discover_started.elapsed();
    let mut read = Duration::ZERO;
    let mut parse = Duration::ZERO;
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    for region in regions {
        let read_started = Instant::now();
        let outcome = read_with_mtime(&region.path);
        read += read_started.elapsed();
        let (bytes, mtime_ms) = match outcome {
            Ok(parts) => parts,
            Err(source) => {
                skipped.push(ScanSkip {
                    path: region.path.clone(),
                    reason: source.to_string(),
                });
                continue;
            }
        };
        let parse_started = Instant::now();
        let entry = parse_entry(&region, &bytes, mtime_ms);
        parse += parse_started.elapsed();
        match entry {
            Ok(entry) => out.push(entry),
            Err(source) => skipped.push(ScanSkip {
                path: region.path.clone(),
                reason: source.to_string(),
            }),
        }
    }
    let timings = ScanTimings {
        total: total_started.elapsed(),
        discover,
        read,
        parse,
    };
    Ok(ScanReport {
        entries: out,
        skipped,
        timings,
    })
}

/// Hash one file's contents and count its chunks.
fn parse_entry(
    region: &RegionRef,
    bytes: &[u8],
    mtime_ms: Option<u64>,
) -> Result<RegionScanEntry, WorldError> {
    let file_bytes = bytes.len() as u64;
    let content_hash = hex_hash(&content_hash_of_bytes(bytes));
    let chunks = count_chunks(bytes, region.region_x, region.region_z)
        .map_err(|source| WorldError::io(&region.path, std::io::Error::other(source)))?;
    Ok(RegionScanEntry {
        path: region.path.clone(),
        dim: region.dim,
        kind: region.kind,
        region_x: region.region_x,
        region_z: region.region_z,
        file_bytes,
        mtime_ms,
        chunks,
        content_hash,
    })
}

/// Upper bound on the pre-allocation for a region read. A declared file
/// size is untrusted: reserving it verbatim would abort the process on a
/// sparse or hostile `.mca` instead of reporting a clean error, so the
/// buffer grows with the bytes actually read.
const MAX_READ_RESERVE: u64 = 8 * 1024 * 1024;

/// Read a whole region file plus its mtime.
fn read_with_mtime(path: &Path) -> Result<(Vec<u8>, Option<u64>), WorldError> {
    let mut file = File::open(path).map_err(|e| WorldError::io(path, e))?;
    let meta = file.metadata().ok();
    let mtime_ms = meta.as_ref().and_then(mtime_ms_from_metadata);
    let reserve = meta
        .as_ref()
        .map_or(0, std::fs::Metadata::len)
        .min(MAX_READ_RESERVE);
    let mut bytes = Vec::with_capacity(usize::try_from(reserve).unwrap_or(0));
    file.read_to_end(&mut bytes)
        .map_err(|e| WorldError::io(path, e))?;
    Ok((bytes, mtime_ms))
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
