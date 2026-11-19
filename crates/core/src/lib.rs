//! Domain and use-case API for commit metadata and content hashes.

#![no_std]

extern crate alloc;

pub mod commit;
#[cfg(test)]
pub(crate) mod support;
pub mod tree;
pub mod usecase;

pub use sekai_util::{
    ApplyOutcome, Area, BytesHasher, ChunkCoord, ChunkHash, Commit, CommitEntry, CommitId,
    CommitRef, DEFAULT_REF_NAME, DimHash, Dimension, Head, HeadState, HexError, NamedScope,
    NamedScopeError, Rect, Ref, RefKind, RefName, RefNameError, ReflogEntry, ReflogNote,
    RegionFingerprint, RegionHash, RegionKey, RegionKind, RootHash, Scope, ScopeName,
    ScopeNameError, UnknownRegionKind, Worktree, hash_bytes,
};

pub use usecase::{
    Assembled, CheckoutError, CheckoutPlan, CheckoutReport, CommitError, CommitReport, CommitStats,
    Plan, Previous, ResolveError, commit_ancestors, commit_history, commit_parents, head,
    list_refs, list_scopes,
};

pub use commit::commit_hash;
