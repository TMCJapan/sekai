//! Rollback execution: rebuild world files from a resolved snapshot plan.
//!
//! Rationale: the restore set (which blobs each region file needs) comes
//! from the [`core`](sekai_core::usecase::rollback) plan; what stays here
//! is mechanics the core cannot own: world-layout discovery, path
//! derivation, directory creation, file deletion, and the atomic MCA
//! rewrite itself. Region files are rebuilt wholesale from CAS blobs (never
//! patched), so on-disk chunks unknown to the snapshot (created later)
//! vanish along with post-snapshot regions, and all-tombstone regions
//! delete their file instead of leaving a header-only shell. A blob missing
//! from CAS aborts loudly: that is corruption, and writing a partial world
//! would be worse than writing none.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sekai_core::usecase::rollback::{RollbackReport, plan_rollback};
use sekai_core::{RegionKey, RegionWriter as _, SnapshotId};
use sekai_storage::Store;

use crate::error::Error;

/// Rebuild `world` from `snapshot` in `store`.
pub fn rollback(
    world: &Path,
    store: &mut Store,
    snapshot: SnapshotId,
) -> Result<RollbackReport, Error> {
    // Snapshot must exist; its timestamp stamps rebuilt files.
    let plan = plan_rollback(store.meta(), snapshot)?;
    let timestamp = u32::try_from(plan.created_at_ms / 1000).unwrap_or(u32::MAX);

    let flavor = sekai_mca::detect_flavor(world);
    let discovered: BTreeMap<RegionKey, PathBuf> = sekai_mca::discover(world)?
        .into_iter()
        .map(|r| {
            (
                RegionKey::new(r.dim, r.kind, r.region_x, r.region_z),
                r.path,
            )
        })
        .collect();
    let mut keys: BTreeSet<RegionKey> = discovered.keys().copied().collect();
    keys.extend(plan.groups.keys().copied());

    let mut report = RollbackReport {
        files_written: 0,
        files_deleted: 0,
        chunks_restored: 0,
    };
    let mut blob_buf = Vec::new();
    for key in keys {
        let rows = plan.groups.get(&key);
        let path = if let Some(path) = discovered.get(&key) {
            path.clone()
        } else {
            if rows.is_none() {
                // No rows and no file: nothing to do.
                continue;
            }
            sekai_mca::derive_path(world, flavor, key.dim, key.kind, key.rx, key.rz)?
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
        let mut writer = sekai_mca::RegionFileWriter::create(&path, key.dim, key.kind, timestamp)?;
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
fn fs_remove(path: &Path) -> Result<(), Error> {
    std::fs::remove_file(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// `fs::create_dir_all` with path-carrying errors.
fn fs_create_dir(path: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}
