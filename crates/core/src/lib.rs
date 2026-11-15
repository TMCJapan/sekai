//! Domain and use-case API for commit metadata and content hashes.

#![no_std]

extern crate alloc;

pub mod commit;
pub mod port;
#[cfg(test)]
pub(crate) mod support;
pub mod tree;
pub mod usecase;

pub use port::{BlobStore, MetaStore};
pub use sekai_util::{
    ApplyOutcome, Area, BytesHasher, ChunkCoord, ChunkHash, Commit, CommitEntry, CommitId,
    CommitRef, DEFAULT_REF_NAME, DimHash, Dimension, HexError, NamedScope, NamedScopeError, Rect,
    RefName, RefNameError, RegionFingerprint, RegionHash, RegionKey, RegionKind, RootHash, Scope,
    ScopeName, ScopeNameError, UnknownRegionKind, hash_bytes,
};
pub use tree::{
    DimEntry, DimNode, NodeLevel, RegionEntry, RegionNode, RootEntry, RootNode, TreeError,
    TreeNodes, empty_tree,
};

pub use usecase::{
    Assembled, CheckoutError, CheckoutPlan, CheckoutReport, CommitError, CommitReport, CommitStats,
    Plan, Previous, ResolveError, active_ref, commit_ancestors, commit_history, commit_parents,
    list_refs, list_scopes,
};

pub use commit::commit_hash;
