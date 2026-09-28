//! Shared filesystem observation helpers: mtime and content hashing.

use std::fs::Metadata;
use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};

/// Chunk size for the streaming content hash. Large enough to keep syscall
/// overhead negligible, small enough to stay off the heap's slow path.
const HASH_CHUNK: usize = 128 * 1024;

pub(crate) fn mtime_ms_from_system_time(t: SystemTime) -> Option<u64> {
    let elapsed = t.duration_since(UNIX_EPOCH).ok()?;
    Some(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}

pub(crate) fn mtime_ms_from_metadata(meta: &Metadata) -> Option<u64> {
    mtime_ms_from_system_time(meta.modified().ok()?)
}

/// Blake3 over the whole stream, read in bounded chunks.
///
/// The hash is what makes "unchanged" mean unchanged: a location-table
/// prefix cannot see a payload edit, and mtime is not trustworthy when a
/// copy, an archive extraction, or a filesystem snapshot preserves it.
pub(crate) fn content_hash_of(reader: &mut impl Read) -> std::io::Result<[u8; 32]> {
    let mut hasher = sekai_anvil::ContentHasher::new();
    let mut buf = vec![0u8; HASH_CHUNK];
    loop {
        let read = reader.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hasher.finalize())
}

pub(crate) fn content_hash_of_bytes(bytes: &[u8]) -> [u8; 32] {
    sekai_anvil::content_hash(bytes)
}

pub(crate) fn hex_hash(bytes: &[u8; 32]) -> String {
    use core::fmt::Write;
    let mut out = String::with_capacity(64);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}
