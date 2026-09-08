//! Rollback planning: resolve one snapshot's captured payloads.
//!
//! Rationale: rollback is strict - after it returns, the world matches the
//! snapshot's captured raw payloads exactly (volatile tags such as
//! `LastUpdate` are rewound to their capture-time values along with
//! everything else). This module resolves *what* must be restored (present
//! rows grouped by region file); the concrete adapter rebuilds files from
//! CAS blobs (never patched), removes files unknown to the snapshot, and
//! deletes rather than shells files whose rows are all tombstones. A blob
//! missing from CAS aborts loudly: that is corruption, and writing a
//! partial world would be worse than writing none.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::fmt;

use super::backup::region_key_of;
use crate::domain::coords::ChunkCoord;
use crate::domain::hash::BlobHash;
use crate::domain::region::RegionKey;
use crate::domain::snapshot::SnapshotId;
use crate::port::meta::MetaStore;

/// Outcome of one rollback run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RollbackReport {
    /// Region files rewritten.
    pub files_written: usize,
    /// Region files deleted (post-snapshot or fully tombstoned).
    pub files_deleted: usize,
    /// Chunks restored from CAS blobs.
    pub chunks_restored: usize,
}

/// Resolved restore set for one snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackPlan {
    /// Snapshot creation time (unix millis, informational only).
    pub created_at_ms: u64,
    /// Present rows grouped by region file.
    pub groups: BTreeMap<RegionKey, Vec<(ChunkCoord, BlobHash)>>,
}

/// Failures while resolving a rollback plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RollbackError<M> {
    /// Metadata backend failure.
    Meta(M),
    /// No snapshot with this ID exists.
    UnknownSnapshot {
        /// Requested snapshot ID.
        id: u64,
    },
}

impl<M: fmt::Debug> fmt::Display for RollbackError<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Meta(_) => write!(f, "snapshot metadata failed"),
            Self::UnknownSnapshot { id } => write!(f, "unknown snapshot: {id}"),
        }
    }
}

/// Resolve the present rows of `snapshot` grouped by region file.
///
/// Tombstones contribute no rows: the adapter deletes files with no group
/// (post-snapshot files and fully tombstoned regions alike).
pub fn plan_rollback<M: MetaStore>(
    meta: &M,
    snapshot: SnapshotId,
) -> Result<RollbackPlan, RollbackError<M::Error>> {
    let created_at_ms = meta
        .lookup_snapshot(snapshot)
        .map_err(RollbackError::Meta)?
        .map(|s| s.created_at_ms)
        .ok_or(RollbackError::UnknownSnapshot { id: snapshot.0 })?;
    let mut groups: BTreeMap<RegionKey, Vec<(ChunkCoord, BlobHash)>> = BTreeMap::new();
    meta.visit_snapshot_chunks(snapshot, |entry| {
        if let Some(blob) = entry.blob {
            groups
                .entry(region_key_of(entry.coord))
                .or_default()
                .push((entry.coord, blob));
        }
        true
    })
    .map_err(RollbackError::Meta)?;
    Ok(RollbackPlan {
        created_at_ms,
        groups,
    })
}
