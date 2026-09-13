//! Application use cases: backup, rollback, GC, and snapshot reads.
//!
//! Rationale: use cases enforce application-level invariants and coordinate
//! ports. They own ordering and policy (what to ingest, carry, tombstone,
//! restore, or reclaim) while concrete adapters own SQLite, CAS layout,
//! Anvil sectors, NBT codecs, threading, and clocks.Filesystem and
//! process mechanics stay at the composition root; the policy here is
//! portable (`no_std` + `alloc` only) and tested without I/O.

pub mod backup;
pub mod gc;
pub mod rollback;
pub mod snapshot;

pub use backup::{Assembled, BackupReport, Observation, Plan, Previous};
pub use gc::{GcError, GcReport};
pub use rollback::{RollbackError, RollbackPlan, RollbackReport};
