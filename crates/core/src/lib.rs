//! Domain and use-case API for backup metadata and content hashes.

#![no_std]

extern crate alloc;

pub mod domain;
pub mod port;
#[cfg(test)]
pub(crate) mod support;
pub mod usecase;

pub use domain::{
    ApplyOutcome, BlobHash, ChunkCoord, ChunkHistoryEntry, CoreError, DiffHash, Dimension, GcPlan,
    RegionFingerprint, RegionKey, RegionKind, RegionStateEntry, Snapshot, SnapshotEntry,
    SnapshotId, hash_blob, resolve_custom_dimension,
};
pub use port::{BlobStore, MetaStore};
pub use usecase::{
    Assembled, BackupReport, GcError, GcReport, Observation, Plan, Previous, RollbackError,
    RollbackPlan, RollbackReport,
};

/// Compute a volatile diff hash for decompressed chunk NBT.
pub fn diff_hash(raw_nbt: &[u8], ignore: &[&str]) -> Result<DiffHash, sekai_nbt::NbtError> {
    sekai_nbt::diff_hash(raw_nbt, ignore).map(DiffHash)
}

/// Compute a diff hash using the default ignore set.
pub fn diff_hash_v1(raw_nbt: &[u8]) -> Result<DiffHash, sekai_nbt::NbtError> {
    sekai_nbt::diff_hash_v1(raw_nbt).map(DiffHash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_wiring_ignores_volatile_tags() {
        let varied = nbt_bytes(100, "minecraft:full");
        let same = nbt_bytes(999_999, "minecraft:full");
        let other = nbt_bytes(100, "minecraft:empty");
        assert_eq!(diff_hash_v1(&varied), diff_hash_v1(&same));
        assert_ne!(diff_hash_v1(&varied), diff_hash_v1(&other));
        assert_ne!(
            diff_hash(&varied, &[]),
            diff_hash(&same, &[]),
            "without ignores the volatile tag surfaces"
        );
    }

    /// Hand-built NBT: compound root with a `long` and a `string` entry.
    fn nbt_bytes(last_update: i64, status: &str) -> alloc::vec::Vec<u8> {
        let mut out = alloc::vec![10, 0, 0];
        out.extend_from_slice(&[4]);
        out.extend_from_slice(&10u16.to_be_bytes());
        out.extend_from_slice(b"LastUpdate");
        out.extend_from_slice(&last_update.to_be_bytes());
        out.extend_from_slice(&[8]);
        out.extend_from_slice(&6u16.to_be_bytes());
        out.extend_from_slice(b"Status");
        let status_len = u16::try_from(status.len()).unwrap();
        out.extend_from_slice(&status_len.to_be_bytes());
        out.extend_from_slice(status.as_bytes());
        out.push(0);
        out
    }
}
