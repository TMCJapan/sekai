//! Shared filesystem observation helpers: mtime and header hashing.

use std::fs::Metadata;
use std::time::{SystemTime, UNIX_EPOCH};

/// First bytes of a region file covered by the header hash.
pub const HEADER_HASH_LEN: usize = 4096;

pub(crate) fn mtime_ms_from_system_time(t: SystemTime) -> Option<u64> {
    let elapsed = t.duration_since(UNIX_EPOCH).ok()?;
    Some(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}

pub(crate) fn mtime_ms_from_metadata(meta: &Metadata) -> Option<u64> {
    mtime_ms_from_system_time(meta.modified().ok()?)
}

pub(crate) fn header_hash_of_prefix(bytes: &[u8]) -> [u8; 32] {
    let prefix_len = bytes.len().min(HEADER_HASH_LEN);
    sekai_anvil::header_hash(&bytes[..prefix_len])
}

pub(crate) fn hex_hash(bytes: &[u8; 32]) -> String {
    use core::fmt::Write;
    let mut out = String::with_capacity(64);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}
