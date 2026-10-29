use sekai_util::DiffHash;

use crate::ChunkPayload;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ChunkPayloadDiff {
    Full { old: DiffHash, new: DiffHash },
    Nothing,
}

impl ChunkPayloadDiff {
    /// Compares two payloads through their normalized view.
    ///
    /// # Errors
    ///
    /// Propagates failures from re-serializing the normalized view.
    pub fn new<'de>(
        old: &ChunkPayload<'de>,
        new: &ChunkPayload<'de>,
    ) -> Result<Self, nanonbt::Error> {
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
