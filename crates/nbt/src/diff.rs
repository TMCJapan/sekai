use sekai_util::ChunkHash;

use crate::Chunk;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ChunkPayloadDiff {
    Full { old: ChunkHash, new: ChunkHash },
    Nothing,
}

impl ChunkPayloadDiff {
    /// Compares the two sides of a chunk by the digests already computed for
    /// them, without decoding either payload.
    ///
    /// `None` means that side holds no chunk. Absent on both sides is
    /// [`Self::Nothing`]; present on exactly one side is [`Self::Full`], with
    /// [`ChunkHash::ZERO`] standing for the absent side, since no content
    /// digest can compare equal to it.
    pub fn from_sides(old: Option<ChunkHash>, new: Option<ChunkHash>) -> Self {
        match (old, new) {
            (Some(old), Some(new)) => Self::changed(old, new),
            (Some(old), None) => Self::changed(old, ChunkHash::ZERO),
            (None, Some(new)) => Self::changed(ChunkHash::ZERO, new),
            (None, None) => Self::Nothing,
        }
    }

    /// The one definition of "changed": two digests differing.
    fn changed(old: ChunkHash, new: ChunkHash) -> Self {
        if old == new {
            Self::Nothing
        } else {
            Self::Full { old, new }
        }
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
    pub const fn is_empty(&self) -> bool {
        matches!(self, Self::Nothing)
    }
}
