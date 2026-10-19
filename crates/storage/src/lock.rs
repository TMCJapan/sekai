//! Store-wide exclusion between processes through OS file locks.
//!
//! Three hazards need it. A backup puts blobs into the CAS *before* it
//! commits the rows that reference them, so a `gc` scanning in between
//! unlinks live blobs (ADR-0011). Two backups race on snapshot IDs. And
//! `prune` deletes rows a reader may have resolved moments earlier, so a
//! rollback could restore a partial snapshot.
//!
//! Two lock files in the store root encode the resulting matrix:
//! `writer.lock` serializes the runs that change what blobs are needed
//! (backup, gc, prune), and `store.lock` keeps readers out only while rows
//! disappear (prune). Readers and backups therefore never block each
//! other. The files are never deleted: removing a held lock file would let
//! the next process lock a fresh inode and walk straight past the holder.
//!
//! `flock`/`LockFileEx` locks are released by the OS when the holding
//! process dies, so a crashed run can never wedge the store. See ADR-0014.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

use crate::api::{StorageError, io_error};

const WRITER_LOCK: &str = "writer.lock";
const STORE_LOCK: &str = "store.lock";

/// How an operation uses the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreLockMode {
    /// Reads and tag edits: run alongside each other and alongside backups.
    Shared,
    /// Runs that write or collect blobs (`backup`, `gc`, store creation):
    /// one at a time, while readers keep going.
    Writer,
    /// Runs that delete rows (`prune`): nothing else may run.
    Exclusive,
}

/// Held for as long as an operation needs the store in its mode.
#[derive(Debug)]
pub struct StoreLock {
    _writer: Option<HeldLock>,
    _store: HeldLock,
}

/// Lock `root` in `mode`, failing immediately when another holder
/// conflicts.
///
/// Locks are per handle, so two operations in the same process conflict
/// exactly like two processes do.
pub fn lock_store(root: impl AsRef<Path>, mode: StoreLockMode) -> Result<StoreLock, StorageError> {
    let root = root.as_ref();
    let writer = match mode {
        StoreLockMode::Shared => None,
        StoreLockMode::Writer | StoreLockMode::Exclusive => {
            Some(HeldLock::acquire(&root.join(WRITER_LOCK), true)?)
        }
    };
    let store = HeldLock::acquire(&root.join(STORE_LOCK), mode == StoreLockMode::Exclusive)?;
    Ok(StoreLock {
        _writer: writer,
        _store: store,
    })
}

#[derive(Debug)]
struct HeldLock(File);

impl HeldLock {
    fn acquire(path: &Path, exclusive: bool) -> Result<Self, StorageError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|source| io_error(path, source))?;
        let locked = if exclusive {
            file.try_lock()
        } else {
            file.try_lock_shared()
        };
        match locked {
            Ok(()) => Ok(Self(file)),
            Err(TryLockError::WouldBlock) => Err(StorageError::StoreLocked {
                path: path.to_path_buf(),
            }),
            Err(TryLockError::Error(source)) => Err(io_error(path, source)),
        }
    }
}

impl Drop for HeldLock {
    fn drop(&mut self) {
        // Closing the handle releases the lock as well, but Windows may do
        // so lazily after the close; unlock eagerly so the next operation
        // in this process does not trip over a lock that is already gone.
        let _ = self.0.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use StoreLockMode::{Exclusive, Shared, Writer};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sekai-lock-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn modes_follow_the_compatibility_matrix() {
        let cases = [
            (Shared, Shared, true),
            (Shared, Writer, true),
            (Writer, Shared, true),
            (Shared, Exclusive, false),
            (Exclusive, Shared, false),
            (Writer, Writer, false),
            (Writer, Exclusive, false),
            (Exclusive, Writer, false),
            (Exclusive, Exclusive, false),
        ];
        for (held, wanted, compatible) in cases {
            let root = temp_root("matrix");
            let _held = lock_store(&root, held).unwrap();
            let second = lock_store(&root, wanted);
            assert_eq!(
                second.is_ok(),
                compatible,
                "{wanted:?} while {held:?} is held"
            );
            if !compatible {
                assert!(matches!(second, Err(StorageError::StoreLocked { .. })));
            }
            let _ = fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn dropping_a_lock_releases_it() {
        let root = temp_root("release");
        let held = lock_store(&root, Exclusive).unwrap();
        assert!(lock_store(&root, Shared).is_err());
        drop(held);
        lock_store(&root, Exclusive).unwrap();
        // Lock files persist: deleting one would split future holders.
        assert!(root.join(WRITER_LOCK).is_file());
        assert!(root.join(STORE_LOCK).is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_failed_exclusive_attempt_releases_the_writer_lock() {
        let root = temp_root("partial");
        let _reader = lock_store(&root, Shared).unwrap();
        // Exclusive takes `writer.lock` before failing on `store.lock`; a
        // writer can only follow if that failure released it again.
        assert!(lock_store(&root, Exclusive).is_err());
        lock_store(&root, Writer).unwrap();
        let _ = fs::remove_dir_all(&root);
    }
}
