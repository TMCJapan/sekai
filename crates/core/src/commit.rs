//! Content-addressed commit hashes.
//!
//! A commit is identified by a Blake3 hash over its logical contents: its
//! parents, creation time, and root snapshot tree. Parents are sorted and
//! deduplicated before hashing, so duplicate or reordered input cannot change
//! the ID. Two histories with the same parents, time, and tree therefore
//! produce the same commit ID, just as Git hashes the same tree, parents, and
//! metadata to the same commit.

use alloc::vec::Vec;

use sekai_util::{CommitId, RootHash, hash_bytes};

const DOMAIN: &[u8] = b"sekai-commit-v2\0";

/// Compute the hash for one commit.
///
/// `parents` is sorted and deduplicated canonically, so the result does not
/// depend on input order or duplicate values. It is empty for the root of a
/// history.
pub fn commit_hash(parents: &[CommitId], created_at_ms: u64, root: RootHash) -> CommitId {
    let mut sorted_parents: Vec<CommitId> = parents.to_vec();
    sorted_parents.sort();
    sorted_parents.dedup();

    let mut input = Vec::with_capacity(DOMAIN.len() + 8 + sorted_parents.len() * 32 + 8 + 32);
    input.extend_from_slice(DOMAIN);
    input.extend_from_slice(&count_bytes(sorted_parents.len()));
    for parent in &sorted_parents {
        input.extend_from_slice(&parent.0);
    }
    input.extend_from_slice(&created_at_ms.to_be_bytes());
    input.extend_from_slice(root.as_bytes());

    CommitId(hash_bytes(&input))
}

fn count_bytes(count: usize) -> [u8; 8] {
    debug_assert!(
        u64::try_from(count).is_ok(),
        "commit list length fits in u64"
    );
    u64::try_from(count).unwrap_or(u64::MAX).to_be_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(byte: u8) -> RootHash {
        RootHash::new([byte; 32])
    }

    #[test]
    fn identical_inputs_produce_identical_ids() {
        let a = commit_hash(&[], 1_000, root(1));
        let b = commit_hash(&[], 1_000, root(1));
        assert_eq!(a, b);
    }

    #[test]
    fn input_order_and_duplicates_do_not_matter() {
        let parent_a = CommitId([1; 32]);
        let parent_b = CommitId([2; 32]);
        let ab = commit_hash(&[parent_a, parent_b], 1_000, root(3));
        let reversed = commit_hash(&[parent_b, parent_a], 1_000, root(3));
        assert_eq!(ab, reversed);
        let repeated = commit_hash(&[parent_a, parent_b, parent_a], 1_000, root(3));
        assert_eq!(ab, repeated);
    }

    #[test]
    fn parent_time_and_root_change_the_id() {
        let base = commit_hash(&[], 1_000, root(1));
        assert_ne!(base, commit_hash(&[CommitId([9; 32])], 1_000, root(1)));
        assert_ne!(base, commit_hash(&[], 1_001, root(1)));
        assert_ne!(base, commit_hash(&[], 1_000, root(2)));
    }
}
