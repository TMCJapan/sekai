//! Application use cases: backup, rollback, GC, snapshot reads, and diffs.
//!
//! Use cases coordinate storage ports and enforce application-level policy.
//! They own ordering and decisions such as carry, tombstone, restore, and GC.
//! Backends own SQLite, CAS layout, threading, and clocks. Policy here is
//! portable (`no_std` + `alloc` only) and tested without I/O.

pub mod backup;
pub mod diff;
pub mod gc;
pub mod rollback;
pub mod snapshot;

pub use backup::{Assembled, BackupReport, Observation, Plan, Previous};
pub use diff::{DiffError, diff_blobs, diff_blobs_v1};
pub use gc::{GcError, GcReport};
pub use rollback::{RollbackError, RollbackPlan, RollbackReport};
