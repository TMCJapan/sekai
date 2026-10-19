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
    pub const FILE_NAME: &str = ".sekai.lock";

    /// Lock the file at `path` in `mode`, failing immediately with
    /// [`io::ErrorKind::WouldBlock`] when another holder conflicts.
    ///
    /// A lock needs no write access, so an existing file this user may not
    /// write - created by another user, or on a read-only mount - is locked
    /// through a read-only handle in either mode. A shared lock degrades to
    /// `Ok(None)` (unguarded) only when the file is absent and cannot be
    /// created, so read-only worlds stay readable. Any other failure,
    /// including an existing file this user cannot open at all, is an
    /// error: silently running unguarded would let a reader overlap another
    /// user's rollback.
    pub fn acquire(path: impl AsRef<Path>, mode: WorldLockMode) -> io::Result<Option<Self>> {
        let path = path.as_ref();
        let opened = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path);
        let file = match opened {
            Ok(file) => file,
            Err(source) if is_read_only(&source) => match File::open(path) {
                Ok(file) => file,
                Err(missing) if missing.kind() == io::ErrorKind::NotFound => {
                    return match mode {
                        WorldLockMode::Shared => Ok(None),
                        WorldLockMode::Exclusive => Err(annotate("open", path, &source)),
                    };
                }
                Err(source) => return Err(annotate("open", path, &source)),
            },
            Err(source) => return Err(annotate("open", path, &source)),
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
            Err(TryLockError::Error(source)) => Err(annotate("take", path, &source)),
        }
    }
}

fn is_read_only(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
    )
}

fn annotate(action: &str, path: &Path, source: &io::Error) -> io::Error {
    io::Error::new(
        source.kind(),
        format!("failed to {action} world lock {}: {source}", path.display()),
    )
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
    fn a_missing_world_is_an_error_not_a_read_only_one() {
        let root = temp_root("missing");
        let path = root.join("absent").join(WorldLock::FILE_NAME);
        assert!(WorldLock::acquire(&path, WorldLockMode::Shared).is_err());
        assert!(WorldLock::acquire(&path, WorldLockMode::Exclusive).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn a_lock_file_another_user_created_still_locks_exclusively() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = temp_root("foreign");
        let path = root.join(WorldLock::FILE_NAME);
        fs::write(&path, "").unwrap();
        // What another user's umask-022 file looks like to this one.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        if OpenOptions::new().write(true).open(&path).is_err() {
            let writer = WorldLock::acquire(&path, WorldLockMode::Exclusive).unwrap();
            assert!(writer.is_some());
            let err = WorldLock::acquire(&path, WorldLockMode::Shared).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn read_only_worlds_stay_readable_and_still_exclude() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = temp_root("read-only");
        let existing = root.join("existing");
        let fresh = root.join("fresh");
        let unreadable = root.join("unreadable");
        for dir in [&existing, &fresh, &unreadable] {
            fs::create_dir_all(dir).unwrap();
        }
        let existing_lock = existing.join(WorldLock::FILE_NAME);
        let unreadable_lock = unreadable.join(WorldLock::FILE_NAME);
        // Taken before the world turns read-only: another user's rollback.
        let writer = WorldLock::acquire(&existing_lock, WorldLockMode::Exclusive)
            .unwrap()
            .unwrap();
        fs::write(&unreadable_lock, "").unwrap();
        fs::set_permissions(&existing_lock, fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(&unreadable_lock, fs::Permissions::from_mode(0o000)).unwrap();
        for dir in [&existing, &fresh, &unreadable] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o555)).unwrap();
        }

        // Privileged users ignore permission bits; nothing to observe then.
        if fs::write(fresh.join("probe"), "").is_err() {
            let err = WorldLock::acquire(&existing_lock, WorldLockMode::Shared).unwrap_err();
            assert_eq!(
                err.kind(),
                io::ErrorKind::WouldBlock,
                "a read-only handle still sees the holder"
            );
            drop(writer);
            let lock = WorldLock::acquire(&existing_lock, WorldLockMode::Shared).unwrap();
            assert!(lock.is_some(), "an existing lock file is still locked");
            let lock = WorldLock::acquire(fresh.join(WorldLock::FILE_NAME), WorldLockMode::Shared);
            assert!(
                lock.unwrap().is_none(),
                "no lock file degrades to unguarded"
            );
            assert!(
                WorldLock::acquire(&unreadable_lock, WorldLockMode::Shared).is_err(),
                "a lock file this user cannot open never degrades"
            );
        }

        for dir in [&existing, &fresh, &unreadable] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let _ = fs::remove_dir_all(&root);
    }
}
