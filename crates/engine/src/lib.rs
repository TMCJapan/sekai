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
mod hash;
mod rollback;
mod store;

pub use backup::{BackupReport, backup};
pub use discover::{LayoutFlavor, RegionRef, derive_path, detect_flavor, discover};
pub use error::EngineError;
pub use hash::Blake3Hasher;
pub use rollback::{RollbackReport, rollback};
pub use store::{Store, list_snapshots};
