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
    ReflogSelector, RegionFingerprint, RegionHash, RegionKey, RegionKind, Revision, RevisionBase,
    RevisionError, RevisionRange, RevisionStep, RootHash, SEKAI_DIR, Scope, ScopeName,
    ScopeNameError, Side, UnknownRegionKind, Worktree, WorktreeName, WorktreeNameError, hash_bytes,
    wildmatch,
};

pub use usecase::{
    Assembled, CheckoutError, CheckoutPlan, CheckoutReport, CommitError, CommitReport, CommitStats,
    FileChange, HeadMove, LogBounds, NewWorktreeHead, Plan, Previous, Reflog, ResolveError, Target,
    WorktreeStart, born_head, branch_holders, checked_out_holder, commit_ancestors, commit_history,
    commit_parents, find_worktree, head, head_branch, head_label, head_reaches, head_state,
    is_ancestor, list_refs, list_scopes, read_reflog, resolve_log_bounds, resolve_reflog,
    resolve_revision, resolve_target, resolve_worktree_head, switch_head, unique_worktree_name,
    walk_log,
};

pub use commit::commit_hash;
