use sekai_util::ChunkHash;

use crate::Chunk;

/// One chunk's normalized view on each side of a diff.
///
/// `None` means that side holds no chunk (absent or tombstoned). The two
/// sides are compared structurally: absent on both is no change, present on
/// exactly one side is a change against `None`, and no sentinel digest is
/// ever fabricated for the missing side.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct ChunkPayloadDiff {
    /// The old side's digest, `None` when it holds no chunk.
    pub old: Option<ChunkHash>,
    /// The new side's digest, `None` when it holds no chunk.
    pub new: Option<ChunkHash>,
}

impl ChunkPayloadDiff {
    /// Compares the two sides of a chunk by the digests already computed for
    /// them, without decoding either payload.
    pub const fn from_sides(old: Option<ChunkHash>, new: Option<ChunkHash>) -> Self {
        Self { old, new }
    }

    /// Compares two chunks through the normalized view of their format.
    ///
    /// A legacy chunk and a modern chunk never normalize equal: the legacy
    /// view writes keys the modern one never does, so a real conversion
    /// stays visible as a change.
    ///
    /// # Errors
    ///
    /// Propagates failures from re-serializing the normalized view.
    pub fn new(old: &Chunk<'_>, new: &Chunk<'_>) -> Result<Self, nanonbt::Error> {
        Ok(Self::from_sides(
            Some(old.normalize_hash()?),
            Some(new.normalize_hash()?),
        ))
    }

    /// Whether both sides normalize identically: this chunk has no changes.
    pub fn is_empty(&self) -> bool {
        self.old == self.new
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn hash(byte: u8) -> ChunkHash {
        ChunkHash::new([byte; 32])
    }

    #[test]
    fn absent_on_both_sides_is_empty() {
        assert!(ChunkPayloadDiff::from_sides(None, None).is_empty());
    }

    #[test]
    fn identical_digests_are_empty() {
        let diff = ChunkPayloadDiff::from_sides(Some(hash(1)), Some(hash(1)));
        assert!(diff.is_empty());
    }

    #[test]
    fn differing_digests_are_a_change() {
        let diff = ChunkPayloadDiff::from_sides(Some(hash(1)), Some(hash(2)));
        assert!(!diff.is_empty());
        assert_eq!((diff.old, diff.new), (Some(hash(1)), Some(hash(2))));
    }

    #[test]
    fn presence_mismatch_keeps_the_missing_side_none() {
        let removed = ChunkPayloadDiff::from_sides(Some(hash(1)), None);
        assert!(!removed.is_empty());
        assert_eq!((removed.old, removed.new), (Some(hash(1)), None));

        let added = ChunkPayloadDiff::from_sides(None, Some(hash(2)));
        assert!(!added.is_empty());
        assert_eq!((added.old, added.new), (None, Some(hash(2))));
    }
}
