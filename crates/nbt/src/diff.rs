use crate::Chunk;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ChunkPayloadDiff {
    Full { old: [u8; 32], new: [u8; 32] },
    Nothing,
}

impl ChunkPayloadDiff {
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
        let old_hash = old.normalize_hash()?;
        let new_hash = new.normalize_hash()?;
        if old_hash == new_hash {
            Ok(Self::Nothing)
        } else {
            Ok(Self::Full {
                old: old_hash,
                new: new_hash,
            })
        }
    }

    /// Whether both sides normalize identically: this chunk has no changes.
    pub const fn is_empty(&self) -> bool {
        matches!(self, Self::Nothing)
    }
}
