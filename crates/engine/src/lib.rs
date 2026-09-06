#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! Backup and rollback orchestration over region files.
//!
//! Rationale: `engine` owns ordering and policy, never formats. It walks
//! the world (`discover`), hashes raw payloads (`hash`), persists blobs
//! before metadata (`store`), and rebuilds files on rollback. Crash
//! ordering (CAS flush, then one metadata transaction) lives here, in the
//! open, so each step stays independently testable.

mod backup;
mod discover;
mod error;
mod gc;
mod hash;
mod rollback;
mod scan;
mod store;
mod timing;

pub use backup::{BackupReport, backup, backup_with_metrics};
pub use discover::{LayoutFlavor, RegionRef, derive_path, detect_flavor, discover};
pub use error::EngineError;
pub use gc::{GcPlan, GcReport, gc_apply, gc_plan};
pub use hash::Blake3Hasher;
pub use rollback::{RollbackReport, rollback};
pub use scan::{HEADER_HASH_LEN, RegionScanEntry, scan_world};
pub use store::{Store, list_snapshots};
pub use timing::{BackupTimings, RegionTiming};
