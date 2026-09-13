//! Snapshot reads over the metadata port.

use alloc::vec::Vec;

use crate::domain::snapshot::Snapshot;
use crate::port::meta::MetaStore;

/// List all snapshots in ID order (for `list` and pre-flight checks).
pub fn list_snapshots<M: MetaStore>(meta: &M) -> Result<Vec<Snapshot>, M::Error> {
    let mut out = Vec::new();
    meta.visit_snapshots(|s| {
        out.push(*s);
        true
    })?;
    Ok(out)
}
