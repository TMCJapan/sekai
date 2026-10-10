//! Domain and use-case API for backup metadata and content hashes.

#![no_std]

extern crate alloc;

pub mod port;
#[cfg(test)]
pub(crate) mod support;
pub mod usecase;

pub use port::{BlobStore, MetaStore};
pub use sekai_util::{
    ApplyOutcome, Area, BlobHash, ChunkCoord, ChunkHistoryEntry, DiffHash, Dimension, FoldOutcome,
    GcPlan, HexError, Rect, RegionFingerprint, RegionKey, RegionKind, RegionStateEntry, Scope,
    Snapshot, SnapshotEntry, SnapshotId, SnapshotTag, TagName, TagNameError,
};

// sekai-nbt AST diff types
pub use sekai_nbt::{DEFAULT_IGNORED, NbtChange, NbtDiffEntry, Value as NbtValue};

pub use usecase::{
    Assembled, BackupReport, DiffError, GcError, GcReport, Observation, Plan, Previous, PruneError,
    PrunePlan, PruneReport, ResolveError, RollbackError, RollbackPlan, RollbackReport,
    SnapshotStats, diff_blobs, diff_blobs_v1,
};

/// Compute a volatile diff hash for decompressed chunk NBT.
pub use sekai_nbt::diff_hash;

/// Compute a diff hash using the default ignore set.
pub use sekai_nbt::diff_hash_v1;

/// Hash the exact raw chunk payload into its CAS key.
pub fn hash_blob(payload: &[u8]) -> BlobHash {
    BlobHash(*blake3::hash(payload).as_bytes())
}

/// Compute structural diff entries between two raw decompressed NBT payloads.
pub fn diff_nbt(
    old_raw_nbt: &[u8],
    new_raw_nbt: &[u8],
    ignore: &[&str],
) -> Result<alloc::vec::Vec<NbtDiffEntry>, sekai_nbt::NbtError> {
    let old_val = sekai_nbt::parse(old_raw_nbt)?;
    let new_val = sekai_nbt::parse(new_raw_nbt)?;
    Ok(sekai_nbt::diff(&old_val, &new_val, ignore))
}

/// Compute structural diff entries using the default ignore set.
pub fn diff_nbt_v1(
    old_raw_nbt: &[u8],
    new_raw_nbt: &[u8],
) -> Result<alloc::vec::Vec<NbtDiffEntry>, sekai_nbt::NbtError> {
    diff_nbt(old_raw_nbt, new_raw_nbt, DEFAULT_IGNORED)
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

    #[test]
    fn diff_nbt_wiring_computes_ast_diff() {
        let old_raw = nbt_bytes(100, "minecraft:full");
        let new_raw = nbt_bytes(100, "minecraft:empty");

        let diffs = diff_nbt_v1(&old_raw, &new_raw).unwrap();
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].path, "Status");
    }

    #[test]
    fn hash_blob_is_stable() {
        assert_eq!(hash_blob(b"payload"), hash_blob(b"payload"));
        assert_ne!(hash_blob(b"payload"), hash_blob(b"other"));
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
