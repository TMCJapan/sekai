//! Application use cases: backup, rollback, GC, and snapshot reads.
//!
//! Use cases coordinate storage ports and enforce application-level policy.
//! They own ordering and decisions such as carry, tombstone, restore, and GC.
//! restore, or reclaim) while backends own SQLite, CAS layout, threading,
//! and clocks. Policy here is portable (`no_std` + `alloc` only) and tested
//! without I/O.

pub mod backup;
pub mod gc;
pub mod rollback;
pub mod snapshot;

pub use backup::{Assembled, BackupReport, Observation, Plan, Previous};
pub use gc::{GcError, GcReport};
pub use rollback::{RollbackError, RollbackPlan, RollbackReport};
