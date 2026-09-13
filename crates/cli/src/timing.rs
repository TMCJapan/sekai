//! Backup phase timings.
//!
//! Rationale: diagnosing slow backups needs per-phase numbers without
//! changing the backup contract. Timings are collected with
//! `std::time::Instant` (composition is a `std` concern; `core` stays
//! `no_std`) and returned alongside the report, so the normal path pays
//! only a handful of cheap clock reads. JSON rendering is the binary's job;
//! this module only carries `Duration`s.

use std::path::PathBuf;
use std::time::Duration;

/// Per-region slice of one backup run.
#[derive(Debug, Clone)]
pub struct RegionTiming {
    /// Region file path as discovered.
    pub path: PathBuf,
    /// Bytes read for this file (`RegionFile` image length).
    pub bytes: u64,
    /// Chunks visited in this file.
    pub chunks: usize,
    /// Time spent in `RegionFile::open` (file read + parse).
    pub open: Duration,
    /// Time spent visiting chunks, hashing, and writing to CAS.
    pub ingest: Duration,
    /// Subset of `ingest` spent hashing raw payloads.
    pub hash: Duration,
    /// Subset of `ingest` spent in `CAS put` (exists-check + write).
    pub cas: Duration,
}

/// Phase breakdown of one [`backup`](crate::backup) run.
///
/// `hash`/`cas_put` are subsets of `ingest` (per-chunk split inside the
/// visit loop); the remaining top-level phases are disjoint and sum to
/// roughly `total`.
#[derive(Debug, Clone)]
pub struct BackupTimings {
    /// Wall time of the whole run.
    pub total: Duration,
    /// Time spent discovering region files.
    pub discover: Duration,
    /// Time spent loading the previous snapshot's coordinate universe.
    pub universe_load: Duration,
    /// Time spent fingerprinting files (`stat` + header hash).
    pub fingerprint: Duration,
    /// Sum of per-region `RegionFile::open` times (changed regions only).
    pub region_open: Duration,
    /// Sum of per-region visit + hash + CAS times (changed regions only).
    pub ingest: Duration,
    /// Subset of `ingest` spent hashing raw payloads.
    pub hash: Duration,
    /// Subset of `ingest` spent in `CAS put`.
    pub cas_put: Duration,
    /// Chunks hashed and offered to CAS.
    pub cas_checked: usize,
    /// Time spent committing the metadata transaction.
    pub db_apply: Duration,
    /// Region files skipped via fingerprint match.
    pub skipped_regions: usize,
    /// History rows carried over for skipped regions.
    pub carried_chunks: usize,
    /// Per-region details for ingested files, in discovery order.
    pub regions: Vec<RegionTiming>,
}
