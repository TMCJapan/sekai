//! Exclusion between processes over one world folder through an OS file
//! lock.
//!
//! The lock file lives in the folder the caller passed, so two callers
//! naming the same world through different roots (a Bukkit server root
//! versus one of its world folders) do not exclude each other. It is never
//! deleted: removing a held lock file would let the next process lock a
//! fresh inode and walk straight past the holder.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;

use crate::{UnguardedReason, WorldLock, WorldLockMode, WorldLockState};

/// [`WorldLock`] of a [`HostWorldTree`](crate::HostWorldTree): an OS file
/// lock on [`Self::FILE_NAME`] in the world root.
#[derive(Debug)]
pub struct HostWorldLock {
    file: File,
    mode: WorldLockMode,
}

impl WorldLock for HostWorldLock {
    fn mode(&self) -> WorldLockMode {
        self.mode
    }
}

/// Why a world could not be locked.
#[derive(Debug)]
pub enum HostWorldLockError {
    /// Another operation holds a conflicting lock.
    Contended,
    /// The lock file could not be prepared or locked.
    Io(io::Error),
}

impl From<io::Error> for HostWorldLockError {
    fn from(source: io::Error) -> Self {
        Self::Io(source)
    }
}

/// The lock file, as far as it could be prepared.
enum LockFile {
    /// The file is open and ready to be locked.
    Open(File),
    /// The file is absent and cannot be created; the create error says
    /// why.
    Absent(io::Error),
}

impl HostWorldLock {
    pub const FILE_NAME: &str = ".sekai.lock";

    /// Lock the file at `path` in `mode`, failing immediately when another
    /// holder conflicts.
    ///
    /// A lock needs no write access, so an existing file this user may not
    /// write - created by another user, or on a read-only mount - is locked
    /// through a read-only handle in either mode. A shared lock degrades to
    /// unguarded only when the file is absent and cannot be created, so
    /// read-only worlds stay readable. Any other failure, including an
    /// existing file this user cannot open at all or a directory this user
    /// cannot search (where absence cannot be told), is an error: silently
    /// running unguarded would let a reader overlap another user's
    /// rollback.
    pub fn acquire(
        path: impl AsRef<Path>,
        mode: WorldLockMode,
    ) -> Result<WorldLockState<Self>, HostWorldLockError> {
        let path = path.as_ref();
        let file = match open_lock_file(path)? {
            LockFile::Open(file) => file,
            LockFile::Absent(source) => {
                return match mode {
                    WorldLockMode::Shared => {
                        Ok(WorldLockState::Unguarded(UnguardedReason::ReadOnlyWorld))
                    }
                    WorldLockMode::Exclusive => {
                        Err(HostWorldLockError::Io(annotate("create", path, &source)))
                    }
                };
            }
        };
        let locked = match mode {
            WorldLockMode::Shared => file.try_lock_shared(),
            WorldLockMode::Exclusive => file.try_lock(),
        };
        match locked {
            Ok(()) => Ok(WorldLockState::Guarded(Self { file, mode })),
            Err(TryLockError::WouldBlock) => Err(HostWorldLockError::Contended),
            Err(TryLockError::Error(source)) => {
                Err(HostWorldLockError::Io(annotate("lock", path, &source)))
            }
        }
    }
}

/// Open the lock file, creating it when the world allows it.
///
/// Whether the world is read-only is decided by the read-only fallback,
/// not by the create error: a directory this user cannot search refuses
/// the create just like a read-only mount does, but it also refuses the
/// read, so absence stays unknown there.
fn open_lock_file(path: impl AsRef<Path>) -> Result<LockFile, io::Error> {
    let path = path.as_ref();
    let opened = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path);
    match opened {
        Ok(file) => Ok(LockFile::Open(file)),
        Err(source) if is_read_only(&source) => match File::open(path) {
            Ok(file) => Ok(LockFile::Open(file)),
            // The file is absent and the directory says so: the world is
            // read-only.
            Err(missing) if missing.kind() == io::ErrorKind::NotFound => {
                Ok(LockFile::Absent(source))
            }
            Err(missing) => Err(annotate("open", path, &missing)),
        },
        Err(source) => Err(annotate("create", path, &source)),
    }
}

/// Whether the create failed because the world cannot be written.
///
/// A read-only mount and a directory this user may not write look the
/// same here; the read-only fallback in [`open_lock_file`] tells them
/// apart, because only a searchable directory can report that the file is
/// absent.
fn is_read_only(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
    )
}

fn annotate(action: &str, path: impl AsRef<Path>, source: &io::Error) -> io::Error {
    let path = path.as_ref();
    io::Error::new(
        source.kind(),
        format!("failed to {action} world lock {}: {source}", path.display()),
    )
}

impl Drop for HostWorldLock {
    fn drop(&mut self) {
        // Closing the handle releases the lock as well, but Windows may do
        // so lazily after the close; unlock eagerly so the next operation
        // in this process does not trip over a lock that is already gone.
        // `Drop` cannot report a failure, and the close that follows
        // releases the lock either way.
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A private directory under the system temp dir, removed on drop.
    ///
    /// The system temp dir is shared, so a name another run already owns
    /// is skipped instead of removed: the suite must not delete what it
    /// did not create.
    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(name: &str) -> Option<Self> {
            let dir = std::env::temp_dir().join(format!(
                "sekai-world-lock-{name}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            if dir.exists() {
                return None;
            }
            fs::create_dir(&dir).ok()?;
            Some(Self(dir))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A temp directory for `name`, or a skip when the shared temp dir
    /// already holds that name.
    fn temp_root(name: &str) -> Option<TempRoot> {
        let root = TempRoot::new(name);
        if root.is_none() {
            eprintln!("skipped: a leftover temp directory of the same name exists");
        }
        root
    }

    /// The I/O error behind a failed acquisition.
    fn io_error(err: HostWorldLockError) -> io::Error {
        match err {
            HostWorldLockError::Io(source) => source,
            HostWorldLockError::Contended => panic!("expected an I/O failure, got contention"),
        }
    }

    /// Directories that are made read-only and made writable again on
    /// drop, so a failing assertion cannot leave the temp tree behind.
    #[cfg(unix)]
    struct ReadOnlyDirs(Vec<PathBuf>);

    #[cfg(unix)]
    impl Drop for ReadOnlyDirs {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt as _;
            for dir in &self.0 {
                let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o755));
            }
        }
    }

    #[test]
    fn readers_share_and_a_writer_excludes() {
        let Some(root) = temp_root("modes") else {
            return;
        };
        let path = root.path().join(HostWorldLock::FILE_NAME);
        let first = HostWorldLock::acquire(&path, WorldLockMode::Shared)
            .unwrap()
            .guard()
            .expect("a writable world creates its lock file");
        // A second reader holds the world alongside the first.
        let second = HostWorldLock::acquire(&path, WorldLockMode::Shared)
            .unwrap()
            .guard()
            .expect("two readers share the world");
        assert_eq!(first.mode(), WorldLockMode::Shared);
        // A writer is excluded while the readers hold it.
        let err = HostWorldLock::acquire(&path, WorldLockMode::Exclusive).unwrap_err();
        assert!(matches!(err, HostWorldLockError::Contended));
        drop(second);
        let err = HostWorldLock::acquire(&path, WorldLockMode::Exclusive).unwrap_err();
        assert!(matches!(err, HostWorldLockError::Contended));

        drop(first);
        let writer = HostWorldLock::acquire(&path, WorldLockMode::Exclusive)
            .unwrap()
            .guard()
            .expect("the writer takes the world alone");
        // A reader and a second writer are both excluded by the writer.
        for mode in [WorldLockMode::Shared, WorldLockMode::Exclusive] {
            let err = HostWorldLock::acquire(&path, mode).unwrap_err();
            assert!(matches!(err, HostWorldLockError::Contended), "{mode:?}");
        }
        drop(writer);
        let again = HostWorldLock::acquire(&path, WorldLockMode::Exclusive).unwrap();
        assert!(again.guard().is_some(), "the released world locks again");
    }

    #[test]
    fn a_missing_world_is_an_error_not_a_read_only_one() {
        let Some(root) = temp_root("missing") else {
            return;
        };
        let path = root.path().join("absent").join(HostWorldLock::FILE_NAME);
        for mode in [WorldLockMode::Shared, WorldLockMode::Exclusive] {
            let err = io_error(HostWorldLock::acquire(&path, mode).unwrap_err());
            assert_eq!(err.kind(), io::ErrorKind::NotFound, "{mode:?}: {err}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_lock_file_another_user_created_still_locks_exclusively() {
        use std::os::unix::fs::PermissionsExt as _;

        let Some(root) = temp_root("foreign") else {
            return;
        };
        let path = root.path().join(HostWorldLock::FILE_NAME);
        fs::write(&path, "").unwrap();
        // What another user's umask-022 file looks like to this one.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        if OpenOptions::new().write(true).open(&path).is_err() {
            let writer = HostWorldLock::acquire(&path, WorldLockMode::Exclusive)
                .unwrap()
                .guard()
                .expect("an existing lock file is still locked");
            let err = HostWorldLock::acquire(&path, WorldLockMode::Shared).unwrap_err();
            assert!(matches!(err, HostWorldLockError::Contended));
            drop(writer);
        } else {
            eprintln!("skipped: permission bits are not enforced for this user");
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn read_only_worlds_stay_readable_and_still_exclude() {
        use std::os::unix::fs::PermissionsExt as _;

        let Some(root) = temp_root("read-only") else {
            return;
        };
        let existing = root.path().join("existing");
        let fresh = root.path().join("fresh");
        let unreadable = root.path().join("unreadable");
        for dir in [&existing, &fresh, &unreadable] {
            fs::create_dir_all(dir).unwrap();
        }
        let existing_lock = existing.join(HostWorldLock::FILE_NAME);
        let unreadable_lock = unreadable.join(HostWorldLock::FILE_NAME);
        // Taken before the world turns read-only: another user's rollback.
        let writer = HostWorldLock::acquire(&existing_lock, WorldLockMode::Exclusive)
            .unwrap()
            .guard()
            .expect("a writable world creates its lock file");
        fs::write(&unreadable_lock, "").unwrap();
        fs::set_permissions(&existing_lock, fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(&unreadable_lock, fs::Permissions::from_mode(0o000)).unwrap();
        for dir in [&existing, &fresh, &unreadable] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o555)).unwrap();
        }
        let _restore = ReadOnlyDirs(vec![existing, unreadable, fresh.clone()]);

        // Privileged users (and some filesystems) ignore permission bits;
        // nothing to observe then.
        if fs::write(fresh.join("probe"), "").is_err() && fs::File::open(&unreadable_lock).is_err()
        {
            let err = HostWorldLock::acquire(&existing_lock, WorldLockMode::Shared).unwrap_err();
            assert!(
                matches!(err, HostWorldLockError::Contended),
                "a read-only handle still sees the holder"
            );
            drop(writer);
            let lock = HostWorldLock::acquire(&existing_lock, WorldLockMode::Shared).unwrap();
            assert!(
                lock.guard().is_some(),
                "an existing lock file is still locked"
            );
            let lock =
                HostWorldLock::acquire(fresh.join(HostWorldLock::FILE_NAME), WorldLockMode::Shared)
                    .unwrap();
            assert!(
                matches!(
                    lock,
                    WorldLockState::Unguarded(UnguardedReason::ReadOnlyWorld)
                ),
                "no lock file degrades to unguarded"
            );
            let err = HostWorldLock::acquire(&unreadable_lock, WorldLockMode::Shared).unwrap_err();
            assert_eq!(
                io_error(err).kind(),
                io::ErrorKind::PermissionDenied,
                "a lock file this user cannot open never degrades"
            );
        } else {
            eprintln!("skipped: permission bits are not enforced for this user");
        }
    }
}
