//! Application use cases: commit, checkout, commit-graph reads, refs,
//! HEAD and reflogs, ref and revision resolution, and named scopes.
//!
//! Use cases coordinate the storage traits and enforce application-level policy.
//! They own ordering and decisions such as change filtering, snapshot
//! merging, and restore.
//! Backends own SQLite, CAS layout, threading, and clocks. Policy here is
//! portable (`no_std` + `alloc` only) and tested without I/O.

pub(crate) mod messages;

pub mod checkout;
pub mod commit;
pub mod graph;
pub mod head;
pub mod log;
pub mod reflog;
pub mod refs;
pub mod resolve;
pub mod revision;
pub mod scopes;
pub mod switch;

pub use checkout::{
    CheckoutError, CheckoutPlan, CheckoutReport, FileChange, overwritten_regions, unchanged_regions,
};
pub use commit::{Assembled, CommitReport, Plan, Previous};
pub use graph::{
    CommitError, CommitStats, commit_ancestors, commit_history, commit_parents, commit_stats,
    is_ancestor,
};
pub use head::{born_head, head, head_branch, head_label, head_reaches, head_state};
pub use log::{LogBounds, resolve_log_bounds, walk_log};
pub use reflog::{Reflog, read_reflog, resolve_reflog};
pub use refs::{
    branch_holders, checked_out_holder, find_worktree, list_refs, unique_worktree_name,
};
pub use resolve::{ResolveError, resolve_commit_ref, resolve_ref};
pub use revision::resolve_revision;
pub use scopes::list_scopes;
pub use switch::{
    HeadMove, NewWorktreeHead, Target, WorktreeStart, resolve_target, resolve_worktree_head,
    switch_head,
};
