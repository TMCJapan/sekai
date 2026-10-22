//! Application use cases: commit, checkout, GC, commit-graph reads, refs,
//! and diffs.
//!
//! Use cases coordinate storage ports and enforce application-level policy.
//! They own ordering and decisions such as carry, tombstone, restore, and GC.
//! Backends own SQLite, CAS layout, threading, and clocks. Policy here is
//! portable (`no_std` + `alloc` only) and tested without I/O.

pub mod checkout;
pub mod commit;
pub mod diff;
pub mod gc;
pub mod graph;
pub mod prune;
pub mod refs;

pub use checkout::{CheckoutError, CheckoutPlan, CheckoutReport};
pub use commit::{Assembled, CommitReport, Observation, Plan, Previous};
pub use diff::{DiffError, diff_blobs, diff_blobs_v1};
pub use gc::{GcError, GcReport};
pub use graph::{
    CommitError, CommitStats, ResolveError, commit_ancestors, commit_history, commit_parents,
    commit_stats, latest_commit_id, list_commits, list_tags, resolve_commit_ref,
};
pub use prune::{PruneError, PrunePlan, PruneReport, prune_apply, prune_plan, select_retained};
pub use refs::{active_ref, list_refs};
