//! Content-addressed commit hashes.
//!
//! A commit is identified by a Blake3 hash over its logical contents: its
//! parents, creation time, fresh history rows (present or tombstone), and
//! removed regions. Each list is sorted and deduplicated before hashing, so
//! duplicate or reordered input cannot change the ID. Carried regions and derived fingerprints are
//! intentionally excluded: they describe storage representation and
//! observation metadata, not the logical world state. Two histories with
//! the same parents, time, and delta therefore produce the same commit ID,
//! just as Git hashes the same tree, parents, and metadata to the same
//! commit.

use alloc::vec::Vec;

use sekai_util::{CommitEntry, CommitId, RegionKey};

const DOMAIN: &[u8] = b"sekai-commit-v1\0";

/// Compute the hash for one commit transition.
///
/// `parents`, `entries`, and `removed` are sorted and deduplicated
/// canonically, so the result does not depend on input order or duplicate
/// values. `parents` is empty for the root of a history.
pub fn commit_hash(
    parents: &[CommitId],
    created_at_ms: u64,
    entries: &[CommitEntry],
    removed: &[RegionKey],
) -> CommitId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(DOMAIN);

    let mut sorted_parents: Vec<CommitId> = parents.to_vec();
    sorted_parents.sort();
    sorted_parents.dedup();
    hasher.update(&count_bytes(sorted_parents.len()));
    for parent in &sorted_parents {
        hasher.update(&parent.0);
    }
    hasher.update(&created_at_ms.to_be_bytes());

    let mut sorted_entries: Vec<CommitEntry> = entries.to_vec();
    sorted_entries.sort();
    sorted_entries.dedup();
    hasher.update(&count_bytes(sorted_entries.len()));
    for entry in &sorted_entries {
        let coord = entry.coord;
        hasher.update(&coord.dim.raw().to_be_bytes());
        hasher.update(&coord.kind.raw().to_be_bytes());
        hasher.update(&coord.x.to_be_bytes());
        hasher.update(&coord.z.to_be_bytes());
        update_optional_hash(&mut hasher, entry.blob.map(|hash| hash.0));
        update_optional_hash(&mut hasher, entry.diff.map(|hash| hash.0));
    }

    let mut sorted_removed: Vec<RegionKey> = removed.to_vec();
    sorted_removed.sort();
    sorted_removed.dedup();
    hasher.update(&count_bytes(sorted_removed.len()));
    for key in &sorted_removed {
        hasher.update(&key.dim.raw().to_be_bytes());
        hasher.update(&key.kind.raw().to_be_bytes());
        hasher.update(&key.rx.to_be_bytes());
        hasher.update(&key.rz.to_be_bytes());
    }

    CommitId(*hasher.finalize().as_bytes())
}

fn count_bytes(count: usize) -> [u8; 8] {
    u64::try_from(count).unwrap_or(u64::MAX).to_be_bytes()
}

fn update_optional_hash(hasher: &mut blake3::Hasher, hash: Option<[u8; 32]>) {
    match hash {
        Some(bytes) => {
            hasher.update(&[1]);
            hasher.update(&bytes);
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sekai_util::{BlobHash, ChunkCoord, Dimension, RegionKind};

    fn coord(x: i32, z: i32) -> ChunkCoord {
        ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Region, x, z)
    }

    fn entry(x: i32, blob: u8) -> CommitEntry {
        CommitEntry::new(coord(x, 0), Some(BlobHash([blob; 32])), None)
    }

    #[test]
    fn identical_inputs_produce_identical_ids() {
        let entries = [entry(0, 1), entry(1, 2)];
        let a = commit_hash(&[], 1_000, &entries, &[]);
        let b = commit_hash(&[], 1_000, &entries, &[]);
        assert_eq!(a, b);
    }

    #[test]
    fn input_order_and_duplicates_do_not_matter() {
        let a = commit_hash(&[], 1_000, &[entry(0, 1), entry(1, 2)], &[]);
        let b = commit_hash(&[], 1_000, &[entry(1, 2), entry(0, 1)], &[]);
        assert_eq!(a, b);

        let parent_a = CommitId([1; 32]);
        let parent_b = CommitId([2; 32]);
        let ab = commit_hash(&[parent_a, parent_b], 1_000, &[], &[]);
        let reversed = commit_hash(&[parent_b, parent_a], 1_000, &[], &[]);
        assert_eq!(ab, reversed);
        let repeated = commit_hash(&[parent_a, parent_b, parent_a], 1_000, &[], &[]);
        assert_eq!(ab, repeated);

        let duplicate_rows = commit_hash(&[], 1_000, &[entry(0, 1), entry(1, 2), entry(0, 1)], &[]);
        assert_eq!(a, duplicate_rows);

        let key = RegionKey::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
        let removed_once = commit_hash(&[], 1_000, &[entry(0, 1)], &[key]);
        let removed_twice = commit_hash(&[], 1_000, &[entry(0, 1)], &[key, key]);
        assert_eq!(removed_once, removed_twice);
    }

    #[test]
    fn parent_time_delta_and_removals_change_the_id() {
        let base = commit_hash(&[], 1_000, &[entry(0, 1)], &[]);
        assert_ne!(
            base,
            commit_hash(&[CommitId([9; 32])], 1_000, &[entry(0, 1)], &[])
        );
        assert_ne!(base, commit_hash(&[], 1_001, &[entry(0, 1)], &[]));
        assert_ne!(base, commit_hash(&[], 1_000, &[entry(0, 2)], &[]));
        assert_ne!(
            base,
            commit_hash(
                &[],
                1_000,
                &[entry(0, 1)],
                &[RegionKey::new(
                    Dimension::OVERWORLD,
                    RegionKind::Region,
                    0,
                    0
                )]
            )
        );
    }
}
