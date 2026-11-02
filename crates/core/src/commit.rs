//! Content-addressed commit hashes.
//!
//! A commit is identified by a Blake3 hash over its logical contents: its
//! parents, creation time, and fresh history rows (present or tombstone).
//! Each list is sorted and deduplicated before hashing, so duplicate or
//! reordered input cannot change the ID. Two histories with the same
//! parents, time, and delta therefore produce the same commit ID, just as
//! Git hashes the same tree, parents, and metadata to the same commit.

use alloc::vec::Vec;

use sekai_util::{ChunkHash, CommitEntry, CommitId, hash_bytes};

const DOMAIN: &[u8] = b"sekai-commit-v1\0";

/// Compute the hash for one commit transition.
///
/// `parents` and `entries` are sorted and deduplicated canonically, so the
/// result does not depend on input order or duplicate values. `parents` is
/// empty for the root of a history.
pub fn commit_hash(parents: &[CommitId], created_at_ms: u64, entries: &[CommitEntry]) -> CommitId {
    let mut sorted_parents: Vec<CommitId> = parents.to_vec();
    sorted_parents.sort();
    sorted_parents.dedup();
    let mut sorted_entries: Vec<CommitEntry> = entries.to_vec();
    sorted_entries.sort();
    sorted_entries.dedup();

    let mut input = Vec::with_capacity(
        DOMAIN.len() + 8 + sorted_parents.len() * 32 + 8 + sorted_entries.len() * 73,
    );
    input.extend_from_slice(DOMAIN);
    input.extend_from_slice(&count_bytes(sorted_parents.len()));
    for parent in &sorted_parents {
        input.extend_from_slice(&parent.0);
    }
    input.extend_from_slice(&created_at_ms.to_be_bytes());
    input.extend_from_slice(&count_bytes(sorted_entries.len()));
    for entry in &sorted_entries {
        let coord = entry.coord;
        input.extend_from_slice(&coord.dim.raw().to_be_bytes());
        input.extend_from_slice(&coord.kind.raw().to_be_bytes());
        input.extend_from_slice(&coord.x.to_be_bytes());
        input.extend_from_slice(&coord.z.to_be_bytes());
        push_optional_blob(&mut input, entry.blob);
    }

    CommitId(hash_bytes(&input))
}

fn count_bytes(count: usize) -> [u8; 8] {
    debug_assert!(
        u64::try_from(count).is_ok(),
        "commit list length fits in u64"
    );
    u64::try_from(count).unwrap_or(u64::MAX).to_be_bytes()
}

/// Presence-tag the optional blob key so a tombstone and a zero hash differ.
fn push_optional_blob(input: &mut Vec<u8>, chunk: Option<ChunkHash>) {
    match chunk {
        Some(hash) => {
            input.push(1);
            input.extend_from_slice(hash.as_bytes());
        }
        None => input.push(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sekai_util::{ChunkCoord, ChunkHash, Dimension, RegionKind};

    fn coord(x: i32, z: i32) -> ChunkCoord {
        ChunkCoord::new(Dimension::OVERWORLD, RegionKind::Region, x, z)
    }

    fn entry(x: i32, blob: u8) -> CommitEntry {
        CommitEntry::new(coord(x, 0), Some(ChunkHash::new([blob; 32])))
    }

    #[test]
    fn identical_inputs_produce_identical_ids() {
        let entries = [entry(0, 1), entry(1, 2)];
        let a = commit_hash(&[], 1_000, &entries);
        let b = commit_hash(&[], 1_000, &entries);
        assert_eq!(a, b);
    }

    #[test]
    fn input_order_and_duplicates_do_not_matter() {
        let a = commit_hash(&[], 1_000, &[entry(0, 1), entry(1, 2)]);
        let b = commit_hash(&[], 1_000, &[entry(1, 2), entry(0, 1)]);
        assert_eq!(a, b);

        let parent_a = CommitId([1; 32]);
        let parent_b = CommitId([2; 32]);
        let ab = commit_hash(&[parent_a, parent_b], 1_000, &[]);
        let reversed = commit_hash(&[parent_b, parent_a], 1_000, &[]);
        assert_eq!(ab, reversed);
        let repeated = commit_hash(&[parent_a, parent_b, parent_a], 1_000, &[]);
        assert_eq!(ab, repeated);

        let duplicate_rows = commit_hash(&[], 1_000, &[entry(0, 1), entry(1, 2), entry(0, 1)]);
        assert_eq!(a, duplicate_rows);
    }

    #[test]
    fn parent_time_and_delta_change_the_id() {
        let base = commit_hash(&[], 1_000, &[entry(0, 1)]);
        assert_ne!(
            base,
            commit_hash(&[CommitId([9; 32])], 1_000, &[entry(0, 1)])
        );
        assert_ne!(base, commit_hash(&[], 1_001, &[entry(0, 1)]));
        assert_ne!(base, commit_hash(&[], 1_000, &[entry(0, 2)]));
        assert_ne!(base, commit_hash(&[], 1_000, &[entry(0, 1), entry(1, 2)]));
    }
}
