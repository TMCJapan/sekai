//! Composition root: concrete backup and rollback entry points over core
//! use cases.
//!
//! Rationale: the CLI assembles implementations (`storage` blobs and
//! metadata, `mca` region files) and executes [`core`](sekai_core)
//! orchestration. Policy (what to ingest, carry, tombstone, restore, or
//! reclaim) lives in `core`; threading, filesystem walks, clocks, and
//! timing collection live here because `core` is `no_std`.

mod backup;
mod error;
mod rollback;
mod timing;

pub use backup::{backup, backup_with_metrics};
pub use error::Error;
pub use rollback::rollback;
pub use sekai_core::usecase::backup::BackupReport;
pub use sekai_core::usecase::rollback::RollbackReport;
pub use timing::{BackupTimings, RegionTiming};

/// List all snapshots in ID order (for `list` and pre-flight checks).
pub fn list_snapshots(store: &sekai_storage::Store) -> Result<Vec<sekai_core::Snapshot>, Error> {
    Ok(sekai_core::usecase::snapshot::list_snapshots(store.meta())?)
}
