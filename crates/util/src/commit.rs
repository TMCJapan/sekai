//! Commit identifiers and metadata.

use alloc::vec::Vec;
use core::fmt;
use core::str::FromStr;

use crate::error::HexError;
use crate::hash::{hex32_from, hex32_into};

/// Content-addressed commit identifier.
///
/// The identifier is the Blake3 hash of the commit object: its parents,
/// creation time, and the logical delta recorded by the commit. Identical
/// logical histories produce identical IDs, matching Git's commit-hash
/// model. Backends keep a separate local sequence for timeline ordering;
/// that sequence is never part of the public ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommitId(pub [u8; 32]);

impl CommitId {
    /// Raw 32 bytes (e.g. for binary DB columns).
    #[inline]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }

    /// Encode as lowercase hex without allocation.
    pub const fn hex_into(self) -> [u8; 64] {
        hex32_into(self.0)
    }

    /// Encode as a lowercase hex string.
    pub fn hex_string(self) -> alloc::string::String {
        let raw = self.hex_into();
        alloc::string::String::from_utf8(raw.to_vec()).unwrap_or_default()
    }

    /// Parse a 64-byte hexadecimal value.
    pub fn from_hex(input: &[u8]) -> Result<Self, HexError> {
        hex32_from(input).map(Self)
    }
}

impl fmt::Display for CommitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(core::str::from_utf8(&self.hex_into()).map_err(|_| fmt::Error)?)
    }
}

impl FromStr for CommitId {
    type Err = HexError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_hex(s.as_bytes())
    }
}

/// Recorded commit metadata.
///
/// A commit is a node of the history graph: `parents` names the
/// commits it continues. The list is never null - it is empty for the
/// root of a history, holds one entry for an ordinary commit, and
/// several for a merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    /// Commit identifier.
    pub id: CommitId,
    /// Creation time as Unix millis (informational only).
    pub created_at_ms: u64,
    /// Commits this one descends from.
    pub parents: Vec<CommitId>,
}

impl Commit {
    /// Construct commit metadata.
    pub const fn new(id: CommitId, created_at_ms: u64, parents: Vec<CommitId>) -> Self {
        Self {
            id,
            created_at_ms,
            parents,
        }
    }

    /// Whether the commit starts a history (no parents).
    pub const fn is_root(&self) -> bool {
        self.parents.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn commit_ids_and_metadata() {
        let id = CommitId([7; 32]);
        let commit = Commit::new(id, 1_700_000_000_000, alloc::vec![CommitId([6; 32])]);
        assert_eq!(commit.id, id);
        assert_eq!(commit.created_at_ms, 1_700_000_000_000);
        assert_eq!(commit.parents, [CommitId([6; 32])]);
        assert!(!commit.is_root());
        assert!(Commit::new(id, 0, alloc::vec![]).is_root());
        assert!(CommitId([1; 32]) < CommitId([2; 32]));
    }

    #[test]
    fn hex_round_trip_and_display() {
        let id = CommitId([0xAB; 32]);
        let hex = id.hex_string();
        assert_eq!(hex.len(), 64);
        assert_eq!(CommitId::from_str(&hex), Ok(id));
        assert_eq!(id.to_string(), hex);
        assert_eq!(CommitId::from_hex(b"abc"), Err(HexError::BadHexLength(3)));
    }
}
