//! Domain and use-case API for commit metadata and content hashes.

#![no_std]

extern crate alloc;

pub mod commit;
pub mod port;
#[cfg(test)]
pub(crate) mod support;
pub mod usecase;

pub use port::{BlobStore, MetaStore};
pub use sekai_util::{
    ApplyOutcome, Area, BytesHasher, ChunkCoord, ChunkHash, ChunkHistoryEntry, Commit, CommitEntry,
    CommitId, CommitRef, DEFAULT_REF_NAME, Dimension, FoldOutcome, GcPlan, HexError, NamedScope,
    NamedScopeError, Rect, RefName, RefNameError, RegionFingerprint, RegionKey, RegionKind, Scope,
    ScopeName, ScopeNameError, UnknownRegionKind, hash_bytes,
};

pub use usecase::{
    Assembled, CheckoutError, CheckoutPlan, CheckoutReport, CommitError, CommitReport, CommitStats,
    DiffError, GcError, GcReport, Observation, Plan, Previous, PruneError, PrunePlan, PruneReport,
    ResolveError, active_ref, commit_ancestors, commit_history, commit_parents, diff_blobs,
    list_refs, list_scopes,
};

pub use sekai_nbt::{ChunkHashError, ChunkPayloadDiff, chunk_key};

pub use commit::commit_hash;

#[cfg(test)]
mod tests {
    use sekai_nbt::nanonbt::Cesu8;

    use super::*;

    #[test]
    fn volatile_tags_never_reach_the_chunk_hash() {
        let varied = nbt_bytes(100, 0, "minecraft:full");
        let same = nbt_bytes(999_999, 0, "minecraft:full");
        let inhabited = nbt_bytes(100, 4_242, "minecraft:full");
        let other = nbt_bytes(100, 0, "minecraft:empty");

        let hash = |raw: &[u8]| sekai_nbt::parse(raw).unwrap().normalize_hash().unwrap();
        assert_eq!(hash(&varied), hash(&same));
        assert_eq!(hash(&varied), hash(&inhabited));
        assert_ne!(hash(&varied), hash(&other));
    }

    #[test]
    fn chunk_key_is_the_cas_key_for_normalized_content() {
        let varied = nbt_bytes(100, 0, "minecraft:full");
        let same = nbt_bytes(999_999, 0, "minecraft:full");
        let other = nbt_bytes(100, 0, "minecraft:empty");

        let key = |raw: &[u8]| chunk_key(RegionKind::Region, raw).unwrap();
        assert_eq!(key(&varied), key(&same));
        assert_ne!(key(&varied), key(&other));
        // The key is the normalized digest, not a hash of the raw bytes.
        assert_ne!(
            key(&varied),
            ChunkHash::new(sekai_util::hash_bytes(&varied))
        );
    }

    #[test]
    fn chunk_key_refuses_invalid_nbt() {
        assert!(matches!(
            chunk_key(RegionKind::Region, b"not nbt at all"),
            Err(ChunkHashError::Invalid(_))
        ));
        assert!(matches!(
            chunk_key(RegionKind::Entities, b"not nbt at all"),
            Err(ChunkHashError::Invalid(_))
        ));
    }

    #[test]
    fn chunk_diff_reports_full_with_distinct_hashes() {
        let old_raw = nbt_bytes(100, 0, "minecraft:full");
        let new_raw = nbt_bytes(100, 0, "minecraft:empty");

        let old = sekai_nbt::parse(&old_raw).unwrap();
        let new = sekai_nbt::parse(&new_raw).unwrap();
        let diffs = ChunkPayloadDiff::new(&old, &new).unwrap();
        assert!(
            matches!((diffs.old, diffs.new), (Some(old), Some(new)) if old != new),
            "status change must report a full diff with distinct hashes: {diffs:?}"
        );
    }

    #[test]
    fn chunk_diff_reports_nothing_for_identical_payloads() {
        let raw = nbt_bytes(100, 0, "minecraft:full");

        let chunk = sekai_nbt::parse(&raw).unwrap();
        assert!(ChunkPayloadDiff::new(&chunk, &chunk).unwrap().is_empty());
    }

    /// Serialised chunk payload with the given volatile fields.
    fn nbt_bytes(last_update: i64, inhabited_time: i64, status: &str) -> alloc::vec::Vec<u8> {
        let value = sekai_nbt::ChunkPayload {
            data_version: 1,
            x_pos: 0,
            y_pos: 0,
            z_pos: 0,
            inhabited_time,
            last_update,
            sections: alloc::vec![],
            heightmaps: sekai_nbt::Heightmaps::default(),
            status: &Cesu8::from_str(status),
            block_entities: alloc::vec![],
            block_ticks: alloc::vec![],
            fluid_ticks: alloc::vec![],
        };
        sekai_nbt::nanonbt::to_bytes(&value).unwrap()
    }
}
