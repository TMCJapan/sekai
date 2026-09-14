//! Snapshot read operations.

use crate::domain::snapshot::Snapshot;
use crate::port::meta::MetaStore;
use alloc::vec::Vec;

/// List all snapshots in ID order.
pub async fn list_snapshots<M: MetaStore>(meta: &M) -> Result<Vec<Snapshot>, M::Error> {
    let mut out = Vec::new();
    meta.visit_snapshots(|snapshot| {
        out.push(*snapshot);
        true
    })
    .await?;
    Ok(out)
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
