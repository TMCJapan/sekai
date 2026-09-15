//! Snapshot read operations.

use core::fmt;

use crate::port::meta::MetaStore;
use alloc::vec::Vec;
use sekai_util::{Snapshot, SnapshotId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotError<M> {
    /// Metadata backend failure.
    Meta(M),
}

impl<M: fmt::Debug> fmt::Display for SnapshotError<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Meta(_) => write!(f, "snapshot metadata failed"),
        }
    }
}

/// List all snapshots in ID order.
pub async fn list_snapshots<M: MetaStore>(
    meta: &M,
) -> Result<Vec<Snapshot>, SnapshotError<M::Error>> {
    let mut out = Vec::new();
    meta.visit_snapshots(|snapshot| {
        out.push(*snapshot);
        true
    })
    .await
    .map_err(SnapshotError::Meta)?;
    Ok(out)
}

/// Get the latest snapshot ID from the store.
pub async fn latest_snapshot_id<M: MetaStore>(
    meta: &M,
) -> Result<Option<SnapshotId>, SnapshotError<M::Error>> {
    let snapshot = meta.latest_snapshot().await.map_err(SnapshotError::Meta)?;
    Ok(snapshot.map(|s| s.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{MemMeta, block_on};

    #[test]
    fn lists_in_id_order() {
        let mut meta = MemMeta::default();
        block_on(meta.create_snapshot(300)).unwrap();
        block_on(meta.create_snapshot(100)).unwrap();
        let listed = block_on(list_snapshots(&meta)).unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed[0].id < listed[1].id);
    }
}
