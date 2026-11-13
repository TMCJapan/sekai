//! Application use cases: commit, checkout, commit-graph reads, refs,
//! and named scopes.
//!
//! Use cases coordinate storage ports and enforce application-level policy.
//! They own ordering and decisions such as change filtering, snapshot
//! merging, and restore.
//! Backends own SQLite, CAS layout, threading, and clocks. Policy here is
//! portable (`no_std` + `alloc` only) and tested without I/O.

pub mod checkout;
pub mod commit;
pub mod graph;
pub mod refs;
pub mod scopes;

pub use checkout::{CheckoutError, CheckoutPlan, CheckoutReport};
pub use commit::{Assembled, CommitReport, Plan, Previous};
pub use graph::{
    CommitError, CommitStats, ResolveError, commit_ancestors, commit_history, commit_parents,
    commit_stats, latest_commit_id, list_commits, resolve_commit_ref,
};
pub use refs::{active_ref, list_refs};
pub use scopes::list_scopes;
