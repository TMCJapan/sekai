//! Exclusive-run marker: one writing run at a time.
//!
//! `gc` decides what is unreferenced by reading metadata, but a backup puts
//! its blobs into the CAS *before* it commits the rows that reference them.
//! A `gc` that scans in that window sees those blobs as orphans and unlinks
//! them; the backup then commits rows pointing at nothing, and every later
//! restore of that snapshot fails with `blob missing from CAS`.
//!
//! Closing that needs mutual exclusion, not a better heuristic: a grace
//! period only shrinks the window. A marker file is the portable form -
//! [`FileCas::begin_run`] takes it for the duration of a writing run and
//! [`FileCas::ensure_idle`] makes `gc` refuse while it is held.
//!
//! The marker is advisory in one direction only. A crashed run leaves it
//! behind, which blocks `gc` (and says how to clear it) but never a backup:
//! [`FileCas::begin_run`] refreshes the marker instead of refusing, so a
//! stale file cannot wedge the command operators actually run on a timer.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::api::{StorageError, io_error};

/// Marker file name inside the store root.
const RUN_MARKER: &str = "backup.inflight";

/// Held for as long as a run may write blobs. Removing the marker is its
/// `Drop`, so an early return or a panic still releases it.
#[derive(Debug)]
pub struct RunGuard {
    path: PathBuf,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Create (or refresh) the marker under `root`.
pub fn begin_run(root: impl AsRef<Path>) -> Result<RunGuard, StorageError> {
    let path = root.as_ref().join(RUN_MARKER);
    // Truncate an existing marker: a leftover from a crashed run must not
    // block backups, only `gc`.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .map_err(|source| io_error(&path, source))?;
    let stamp = format!("{} {}\n", std::process::id(), unix_secs());
    file.write_all(stamp.as_bytes())
        .map_err(|source| io_error(&path, source))?;
    Ok(RunGuard { path })
}

/// Fail while a run marker is present.
pub fn ensure_idle(root: impl AsRef<Path>) -> Result<(), StorageError> {
    let path = root.as_ref().join(RUN_MARKER);
    match fs::read_to_string(&path) {
        Ok(stamp) => Err(StorageError::StoreBusy {
            path,
            holder: stamp.trim().to_owned(),
        }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error(&path, source)),
    }
}

fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sekai-runs-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_held_run_blocks_collection_and_releases_on_drop() {
        let root = temp_root("held");
        assert!(ensure_idle(&root).is_ok(), "an untouched store is idle");

        let guard = begin_run(&root).unwrap();
        let err = ensure_idle(&root).unwrap_err();
        assert!(matches!(err, StorageError::StoreBusy { .. }));
        // The message must tell an operator how to recover.
        assert!(err.to_string().contains("remove the marker"));

        drop(guard);
        assert!(ensure_idle(&root).is_ok());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_stale_marker_does_not_wedge_backups() {
        let root = temp_root("stale");
        fs::write(root.join(RUN_MARKER), "999999 0\n").unwrap();
        assert!(ensure_idle(&root).is_err());

        let guard = begin_run(&root).unwrap();
        let _ = guard;
        // Still held by this run, and released afterwards.
        assert!(ensure_idle(&root).is_err());
        let _ = fs::remove_dir_all(&root);
    }
}
