//! Rollback: rebuild world files byte-identically from one snapshot.
//!
//! Rationale: rollback is strict - after it returns, the world matches the
//! snapshot exactly. Region files are rebuilt wholesale from CAS blobs
//! (never patched), so on-disk chunks unknown to the snapshot (created
//! later) vanish along with post-snapshot regions, and all-tombstone
//! regions delete their file instead of leaving a header-only shell.
//! A blob missing from CAS aborts loudly: that is corruption, and writing
//! a partial world would be worse than writing none.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sekai_core::{
    BlobHash, ChunkCoord, Dimension, MetaStore as _, RegionKind, RegionWriter as _, SnapshotId,
};

use crate::discover::{derive_path, detect_flavor, discover};
use crate::error::EngineError;
use crate::store::Store;

/// Outcome of one [`rollback`] run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RollbackReport {
    /// Region files rewritten.
    pub files_written: usize,
    /// Region files deleted (post-snapshot or fully tombstoned).
    pub files_deleted: usize,
    /// Chunks restored from CAS blobs.
    pub chunks_restored: usize,
}

/// Rebuild `world` from `snapshot` in `store`.
pub fn rollback(
    world: &Path,
    store: &mut Store,
    snapshot: SnapshotId,
) -> Result<RollbackReport, EngineError> {
    type Key = (Dimension, RegionKind, i32, i32);
    // Snapshot must exist; its timestamp stamps rebuilt files.
    let created_at_ms = store
        .meta()
        .lookup_snapshot(snapshot)?
        .map(|s| s.created_at_ms)
        .ok_or(EngineError::UnknownSnapshot { id: snapshot.0 })?;
    let timestamp = u32::try_from(created_at_ms / 1000).unwrap_or(u32::MAX);

    // Present rows grouped by region file.
    let mut groups: BTreeMap<Key, Vec<(ChunkCoord, BlobHash)>> = BTreeMap::new();
    store.meta().visit_snapshot_chunks(snapshot, |entry| {
        if let Some(blob) = entry.blob {
            groups
                .entry((
                    entry.coord.dim,
                    entry.coord.kind,
                    entry.coord.region_x(),
                    entry.coord.region_z(),
                ))
                .or_default()
                .push((entry.coord, blob));
        }
        true
    })?;

    let flavor = detect_flavor(world);
    let discovered: BTreeMap<Key, PathBuf> = discover(world)?
        .into_iter()
        .map(|r| ((r.dim, r.kind, r.region_x, r.region_z), r.path))
        .collect();
    let mut keys: BTreeSet<Key> = discovered.keys().copied().collect();
    keys.extend(groups.keys().copied());

    let mut report = RollbackReport {
        files_written: 0,
        files_deleted: 0,
        chunks_restored: 0,
    };
    let mut blob_buf = Vec::new();
    for (dim, kind, region_x, region_z) in keys {
        let key = (dim, kind, region_x, region_z);
        let rows = groups.get(&key);
        let path = if let Some(path) = discovered.get(&key) {
            path.clone()
        } else {
            if rows.is_none() {
                // No rows and no file: nothing to do.
                continue;
            }
            derive_path(world, flavor, dim, kind, region_x, region_z)?
        };
        let Some(rows) = rows else {
            // Strict rollback: the snapshot knows nothing of this file,
            // so it was created afterwards - remove it.
            if path.exists() {
                fs_remove(&path)?;
                report.files_deleted += 1;
            }
            continue;
        };
        if let Some(parent) = path.parent()
            && !parent.is_dir()
        {
            fs_create_dir(parent)?;
        }
        let mut writer = sekai_mca::RegionFileWriter::create(&path, dim, kind, timestamp)?;
        for (coord, hash) in rows {
            // Missing blob = corruption: abort, do not write partial worlds.
            sekai_core::BlobStore::fetch_into(store.cas(), hash, &mut blob_buf)?;
            writer.stage_chunk(coord, &blob_buf)?;
            report.chunks_restored += 1;
        }
        writer.commit()?;
        report.files_written += 1;
    }
    Ok(report)
}

/// `fs::remove_file` with path-carrying errors.
fn fs_remove(path: &Path) -> Result<(), EngineError> {
    std::fs::remove_file(path).map_err(|source| EngineError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// `fs::create_dir_all` with path-carrying errors.
fn fs_create_dir(path: &Path) -> Result<(), EngineError> {
    std::fs::create_dir_all(path).map_err(|source| EngineError::Io {
        path: path.to_path_buf(),
        source,
    })
}
