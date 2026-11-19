//! Application use cases: commit, checkout, commit-graph reads, refs,
//! and named scopes.
//!
//! Use cases coordinate the storage traits and enforce application-level policy.
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
    commit_stats, list_commits, resolve_commit_ref,
};
pub use refs::{head, list_refs};
pub use scopes::list_scopes;
