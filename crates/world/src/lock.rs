//! Exclusion between processes over one world folder through an OS file
//! lock.
//!
//! A rollback rewrites region files that a concurrent backup, status, or
//! diff would read, and two rollbacks would interleave their swaps. The
//! lock only coordinates sekai operations: the Minecraft server never
//! takes it, and keeping the server quiet stays the caller's job.
//!
//! The lock file lives in the folder the caller passed, so two callers
//! naming the same world through different roots (a Bukkit server root
//! versus one of its world folders) do not exclude each other. It is never
//! deleted: removing a held lock file would let the next process lock a
//! fresh inode and walk straight past the holder.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;

/// How an operation uses a world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorldLockMode {
    /// Reads (backup, status, diff): run alongside each other.
    Shared,
    /// Writes (rollback): nothing else may touch the world.
    Exclusive,
}

/// Held for as long as an operation needs a world in its mode.
#[derive(Debug)]
pub struct WorldLock(File);

impl WorldLock {
    /// Lock file name inside a world root.
    pub const FILE_NAME: &str = ".sekai.lock";

    /// Lock the file at `path` in `mode`, failing immediately with
    /// [`io::ErrorKind::WouldBlock`] when another holder conflicts.
    ///
    /// A shared lock degrades to `Ok(None)` (unguarded) when the file can
    /// be neither created nor opened, so read-only worlds - snapshot
    /// mounts, backup users without write access - stay readable. An
    /// exclusive lock is taken only by operations that write the world
    /// anyway, so failing to create its file is an error.
    pub fn acquire(path: impl AsRef<Path>, mode: WorldLockMode) -> io::Result<Option<Self>> {
        let path = path.as_ref();
        let opened = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path);
        let file = match (opened, mode) {
            (Ok(file), _) => file,
            (Err(source), WorldLockMode::Exclusive) => {
                return Err(io::Error::new(
                    source.kind(),
                    format!("failed to open world lock {}: {source}", path.display()),
                ));
            }
            (Err(_), WorldLockMode::Shared) => match File::open(path) {
                Ok(file) => file,
                Err(_) => return Ok(None),
            },
        };
        let locked = match mode {
            WorldLockMode::Shared => file.try_lock_shared(),
            WorldLockMode::Exclusive => file.try_lock(),
        };
        match locked {
            Ok(()) => Ok(Some(Self(file))),
            Err(TryLockError::WouldBlock) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "world is in use by another sekai operation (lock {}): retry once it finishes",
                    path.display()
                ),
            )),
            Err(TryLockError::Error(source)) => Err(source),
        }
    }
}

impl Drop for WorldLock {
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

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sekai-world-lock-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn readers_share_and_a_writer_excludes() {
        let root = temp_root("modes");
        let path = root.join(WorldLock::FILE_NAME);
        let first = WorldLock::acquire(&path, WorldLockMode::Shared).unwrap();
        assert!(first.is_some());
        assert!(
            WorldLock::acquire(&path, WorldLockMode::Shared)
                .unwrap()
                .is_some()
        );
        let err = WorldLock::acquire(&path, WorldLockMode::Exclusive).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);

        drop(first);
        let writer = WorldLock::acquire(&path, WorldLockMode::Exclusive).unwrap();
        let err = WorldLock::acquire(&path, WorldLockMode::Shared).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        drop(writer);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_writer_needs_a_lock_file() {
        let root = temp_root("missing");
        let path = root.join("absent").join(WorldLock::FILE_NAME);
        assert!(
            WorldLock::acquire(&path, WorldLockMode::Shared)
                .unwrap()
                .is_none()
        );
        assert!(WorldLock::acquire(&path, WorldLockMode::Exclusive).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn read_only_worlds_stay_readable() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = temp_root("read-only");
        let existing = root.join("existing");
        let fresh = root.join("fresh");
        fs::create_dir_all(&existing).unwrap();
        fs::create_dir_all(&fresh).unwrap();
        fs::write(existing.join(WorldLock::FILE_NAME), "").unwrap();
        for dir in [&existing, &fresh] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o555)).unwrap();
        }
        fs::set_permissions(
            existing.join(WorldLock::FILE_NAME),
            fs::Permissions::from_mode(0o444),
        )
        .unwrap();

        // Privileged users ignore permission bits; nothing to observe then.
        if fs::write(fresh.join("probe"), "").is_err() {
            let lock =
                WorldLock::acquire(existing.join(WorldLock::FILE_NAME), WorldLockMode::Shared);
            assert!(
                lock.unwrap().is_some(),
                "an existing lock file is still honored"
            );
            let lock = WorldLock::acquire(fresh.join(WorldLock::FILE_NAME), WorldLockMode::Shared);
            assert!(
                lock.unwrap().is_none(),
                "no lock file degrades to unguarded"
            );
        }

        for dir in [&existing, &fresh] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let _ = fs::remove_dir_all(&root);
    }
}
