use crate::ChunkPayload;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ChunkPayloadDiff {
    Full {
        old: alloc::boxed::Box<ChunkPayload>,
        new: alloc::boxed::Box<ChunkPayload>,
    },
    Nothing,
}

impl ChunkPayloadDiff {
    pub fn new(old: ChunkPayload, new: ChunkPayload) -> Self {
        if old == new {
            Self::Nothing
        } else {
            Self::Full {
                old: alloc::boxed::Box::new(old),
                new: alloc::boxed::Box::new(new),
            }
        }
    }
    pub const fn is_empty(&self) -> bool {
        matches!(self, Self::Nothing)
    }
}
