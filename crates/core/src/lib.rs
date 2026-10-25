//! Domain and use-case API for commit metadata and content hashes.

#![no_std]

extern crate alloc;

pub mod commit;
pub mod port;
#[cfg(test)]
pub(crate) mod support;
pub mod usecase;

pub use port::{BlobStore, MetaStore};
use sekai_nbt::ChunkPayloadDiff;
pub use sekai_util::{
    ApplyOutcome, Area, BlobHash, ChunkCoord, ChunkHistoryEntry, Commit, CommitEntry, CommitId,
    CommitRef, DEFAULT_REF_NAME, DiffHash, Dimension, FoldOutcome, GcPlan, HexError, NamedScope,
    NamedScopeError, Rect, RefName, RefNameError, RegionFingerprint, RegionKey, RegionKind, Scope,
    ScopeName, ScopeNameError, UnknownRegionKind,
};

pub use usecase::{
    Assembled, CheckoutError, CheckoutPlan, CheckoutReport, CommitError, CommitReport, CommitStats,
    DiffError, GcError, GcReport, Observation, Plan, Previous, PruneError, PrunePlan, PruneReport,
    ResolveError, active_ref, commit_ancestors, commit_history, commit_parents, diff_blobs,
    diff_blobs_v1, list_refs, list_scopes,
};

/// Compute a volatile diff hash for decompressed chunk NBT.
pub use sekai_nbt::diff_hash;

/// Compute a diff hash using the default ignore set.
pub use sekai_nbt::diff_hash_v1;

pub use commit::commit_hash;

/// Hash the exact raw chunk payload into its CAS key.
pub fn hash_blob(payload: &[u8]) -> BlobHash {
    BlobHash(*blake3::hash(payload).as_bytes())
}

/// Compute structural diff entries between two raw decompressed NBT payloads.
pub fn diff_nbt(
    old_raw_nbt: &[u8],
    new_raw_nbt: &[u8],
) -> Result<ChunkPayloadDiff, sekai_nbt::NbtError> {
    let old_val = sekai_nbt::parse(old_raw_nbt)?;
    let new_val = sekai_nbt::parse(new_raw_nbt)?;

    Ok(ChunkPayloadDiff::new(old_val, new_val))
}

/// Compute structural diff entries using the default ignore set.
pub fn diff_nbt_v1(
    old_raw_nbt: &[u8],
    new_raw_nbt: &[u8],
) -> Result<ChunkPayloadDiff, sekai_nbt::NbtError> {
    diff_nbt(old_raw_nbt, new_raw_nbt)
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
    }

    #[test]
    fn diff_nbt_wiring_computes_ast_diff() {
        let old_raw = nbt_bytes(100, "minecraft:full");
        let new_raw = nbt_bytes(100, "minecraft:empty");

        let diffs = diff_nbt_v1(&old_raw, &new_raw).unwrap();
        assert!(!diffs.is_empty());
    }

    #[test]
    fn hash_blob_is_stable() {
        assert_eq!(hash_blob(b"payload"), hash_blob(b"payload"));
        assert_ne!(hash_blob(b"payload"), hash_blob(b"other"));
    }

    /// Hand-built NBT: serialised chunk payload.
    fn nbt_bytes(last_update: i64, status: &str) -> alloc::vec::Vec<u8> {
        let value = sekai_nbt::ChunkPayload {
            data_version: 1,
            x_pos: 0,
            y_pos: 0,
            z_pos: 0,
            inhabited_time: 0,
            last_update,
            sections: alloc::vec![],
            heightmaps: sekai_nbt::Heightmaps {},
            status: status.into(),
            block_entities: alloc::vec![],
        };
        sekai_nbt::nanonbt::to_bytes(&value).unwrap()
    }
}
