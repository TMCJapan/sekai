use sekai_util::DiffHash;

use crate::ChunkPayload;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ChunkPayloadDiff {
    Full { old: DiffHash, new: DiffHash },
    Nothing,
}

impl ChunkPayloadDiff {
    pub fn new<'de>(
        old: &ChunkPayload<'de>,
        new: &ChunkPayload<'de>,
    ) -> Result<Self, nanonbt::Error> {
        if old == new {
            Ok(Self::Nothing)
        } else {
            Ok(Self::Full {
                old: old.normalize_hash()?,
                new: new.normalize_hash()?,
            })
        }
    }
    pub const fn is_empty(&self) -> bool {
        matches!(self, Self::Nothing)
    }
}
