//! Exclusion between processes over one world folder through an OS file
//! lock.
//!
//! The lock file lives in the folder the caller passed, so two callers
//! naming the same world through different roots (a Bukkit server root
//! versus one of its world folders) do not exclude each other. It is never
//! deleted: removing a held lock file would let the next process lock a
//! fresh inode and walk straight past the holder.
//!
//! The lock belongs to the open file description, not to the path, so two
//! handles on the same file conflict exactly like two processes do. That
//! is what the same-process tests rely on.
//!
//! A symlink at the lock path is refused, since locking its target would
//! exclude two holders on different inodes. On Unix the open itself
//! carries `O_NOFOLLOW`, so the refusal does not depend on a check that
//! a later swap can outrun; elsewhere the pre-check is all there is, and
//! a link swapped in between it and the open is not caught.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;

use crate::{UnguardedReason, WorldLock, WorldLockMode, WorldLockState};

/// [`WorldLock`] of a [`HostWorldTree`](crate::HostWorldTree): an OS file
/// lock on [`Self::FILE_NAME`] in the world root.
///
/// The guard holds the lock file open; dropping it releases the lock.
/// The lock is never upgraded in place, so the guard can be moved to
/// another thread and still excludes the same set of holders. The
/// [`WorldLock`] supertraits are what make that move sound.
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

/// The lock file, as far as it could be prepared.
enum LockFile {
    /// The file is open and ready to be locked.
    Open(File),
    /// The file is absent and cannot be created. The create error says
    /// why, but only the exclusive mode reports it: a shared lock
    /// degrades to unguarded and names the reason instead.
    Absent(io::Error),
}

impl HostWorldLock {
    /// Name of the lock file in the world root; it is created there on
    /// first use and never deleted.
    pub const FILE_NAME: &str = ".sekai.lock";

    /// Lock the file at `path` in `mode`, failing immediately when another
    /// holder conflicts.
    ///
    /// A shared lock on a read-only world answers
    /// [`WorldLockState::Unguarded`] instead of a guard: the lock file
    /// cannot be created there, and the world stays readable.
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
        let file = match open_lock_file(path).map_err(HostWorldLockError::Io)? {
            LockFile::Open(file) => file,
            LockFile::Absent(source) => {
                return match mode {
                    WorldLockMode::Shared => {
                        Ok(WorldLockState::Unguarded(UnguardedReason::ReadOnlyWorld))
                    }
                    WorldLockMode::Exclusive => {
                        Err(HostWorldLockError::Io(annotate("create", &source)))
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
                Err(HostWorldLockError::Io(annotate("take", &source)))
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
    // Off Unix there is no `O_NOFOLLOW`, so the pre-check is the only
    // refusal of a symlink; there the open cannot enforce it.
    #[cfg(not(unix))]
    reject_symlink(path).map_err(|source| annotate("open", &source))?;
    match open_lock_path(path) {
        Ok(file) => Ok(LockFile::Open(file)),
        // A symlink is refused on purpose, not a failed create.
        Err(source) if source.kind() == io::ErrorKind::InvalidInput => {
            Err(annotate("open", &source))
        }
        Err(source) if is_read_only(&source) => match open_read_only_lock_path(path) {
            Ok(file) => Ok(LockFile::Open(file)),
            // The file is absent and the directory says so: the world is
            // read-only.
            Err(missing) if missing.kind() == io::ErrorKind::NotFound => {
                Ok(LockFile::Absent(source))
            }
            Err(missing) => Err(annotate("open", &missing)),
        },
        Err(source) => Err(annotate("create", &source)),
    }
}

/// Open (and create) the lock file, refusing to follow a symlink.
fn open_lock_path(path: impl AsRef<Path>) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        // `O_NOFOLLOW` is what enforces the refusal. The check in
        // `reject_symlink` only catches a link that is already there, so
        // a link swapped in afterwards must fail the open itself.
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path).map_err(normalize_symlink)
}

/// Open an existing lock file read-only, refusing to follow a symlink.
///
/// The read-only fallback needs the same refusal as the create: a link
/// swapped in between the two opens would otherwise move the lock onto
/// its target.
fn open_read_only_lock_path(path: impl AsRef<Path>) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path).map_err(normalize_symlink)
}

/// Refuse a symlink at the lock path.
///
/// A link would move the lock onto another file: two holders could then
/// sit on different inodes and neither would exclude the other. Only a
/// positive detection is an error - a path this user cannot even stat is
/// left to the open below, which reports the real failure.
///
/// This is the only refusal where `O_NOFOLLOW` is unavailable. There it
/// is best-effort: it runs before the open, so a link swapped in
/// afterwards is followed.
#[cfg(not(unix))]
fn reject_symlink(path: impl AsRef<Path>) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(symlink_error()),
        _ => Ok(()),
    }
}

/// The error a symlink at the lock path produces.
fn symlink_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "the world lock path is a symlink",
    )
}

/// Report a refused symlink the same way everywhere.
///
/// `O_NOFOLLOW` refuses it with `ELOOP`, while the pre-check reports
/// `InvalidInput`. A caller matching on the kind must not depend on
/// which check won the race.
// Without `O_NOFOLLOW` there is nothing to normalize, so this is
// trivially `const` off Unix.
#[cfg_attr(not(unix), allow(clippy::missing_const_for_fn))]
fn normalize_symlink(source: io::Error) -> io::Error {
    #[cfg(unix)]
    {
        if source.raw_os_error() == Some(libc::ELOOP) {
            return symlink_error();
        }
    }
    source
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

/// Wrap `source` with the step that failed.
///
/// The path is left to the caller: `WorldError::Io` carries it, so
/// repeating it here would render it twice. The original error is
/// rendered into the message rather than kept in the source chain: a
/// chain would report the same OS error a second time under the
/// wrapper, and the kind - the part callers branch on - survives on the
/// wrapping error.
fn annotate(action: &'static str, source: &io::Error) -> io::Error {
    io::Error::new(
        source.kind(),
        format!("failed to {action} the world lock: {source}"),
    )
}

impl Drop for HostWorldLock {
    fn drop(&mut self) {
        // Closing the handle releases the lock as well, but Windows may do
        // so lazily after the close; unlock eagerly so the next operation
        // in this process does not trip over a lock that is already gone.
        // `Drop` cannot report a failure, and the close that follows
        // releases the lock either way. A failed unlock can still leave
        // the next acquire reporting contention until the handle closes.
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
    /// is stepped over rather than removed: the suite must not delete
    /// what it did not create.
    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(name: &str) -> Self {
            for _ in 0..16 {
                let dir = std::env::temp_dir().join(format!(
                    "sekai-world-lock-{name}-{}-{}",
                    std::process::id(),
                    COUNTER.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&dir) {
                    Ok(()) => return Self(dir),
                    // Another run owns the name: step over it.
                    Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(source) => panic!("cannot create {}: {source}", dir.display()),
                }
            }
            panic!("no free temp directory for {name}");
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

    /// The I/O error behind a failed acquisition.
    fn io_error(err: HostWorldLockError) -> io::Error {
        match err {
            HostWorldLockError::Io(source) => source,
            HostWorldLockError::Contended => panic!("expected an I/O failure, got contention"),
        }
    }

    /// Directories that are made unwritable and made writable again on
    /// drop, so a failing assertion cannot leave the temp tree behind.
    #[cfg(unix)]
    struct LockedDirs(Vec<PathBuf>);

    #[cfg(unix)]
    impl Drop for LockedDirs {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt as _;
            for dir in &self.0 {
                let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o755));
            }
        }
    }

    #[test]
    fn readers_share_and_a_writer_excludes() {
        let root = TempRoot::new("modes");
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
        assert!(again.guard().is_ok(), "the released world locks again");
    }

    #[test]
    fn a_missing_world_is_an_error_not_a_read_only_one() {
        let root = TempRoot::new("missing");
        let path = root.path().join("absent").join(HostWorldLock::FILE_NAME);
        for mode in [WorldLockMode::Shared, WorldLockMode::Exclusive] {
            let err = io_error(HostWorldLock::acquire(&path, mode).unwrap_err());
            assert_eq!(err.kind(), io::ErrorKind::NotFound, "{mode:?}: {err}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_lock_file_is_refused() {
        let root = TempRoot::new("symlink");
        let target = root.path().join("target");
        fs::write(&target, "").unwrap();
        let path = root.path().join(HostWorldLock::FILE_NAME);
        std::os::unix::fs::symlink(&target, &path).unwrap();
        // Locking the link's target would exclude two holders on
        // different inodes, so the link is refused instead.
        let err = io_error(HostWorldLock::acquire(&path, WorldLockMode::Shared).unwrap_err());
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn the_open_itself_refuses_a_symlink() {
        // The pre-check only catches a link that is already there; the
        // open is what refuses a link swapped in afterwards.
        let root = TempRoot::new("symlink-open");
        let target = root.path().join("target");
        fs::write(&target, "").unwrap();
        let path = root.path().join(HostWorldLock::FILE_NAME);
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let err = open_lock_path(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn the_read_only_fallback_refuses_a_symlink() {
        // The fallback must not follow a link either: a link swapped in
        // between the two opens would move the lock onto its target.
        let root = TempRoot::new("symlink-read-only");
        let target = root.path().join("target");
        fs::write(&target, "").unwrap();
        let path = root.path().join(HostWorldLock::FILE_NAME);
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let err = open_read_only_lock_path(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn an_unsearchable_world_is_an_error_not_a_read_only_one() {
        use std::os::unix::fs::PermissionsExt as _;

        // A directory this user cannot search refuses the create and the
        // read alike, so the lock file's absence cannot be told: the lock
        // has to fail rather than degrade to an unguarded run.
        let root = TempRoot::new("unsearchable");
        let dir = root.path().join("unsearchable");
        let probe = root.path().join("probe");
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(&probe).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).unwrap();
        fs::set_permissions(&probe, fs::Permissions::from_mode(0o555)).unwrap();
        let _restore = LockedDirs(vec![dir.clone(), probe.clone()]);

        // Privileged users (and some filesystems) ignore permission bits;
        // nothing to observe then. The probe has its own directory, so
        // the unsearchable one stays unsearchable.
        if fs::write(probe.join("file"), "").is_ok() {
            eprintln!("skipped: permission bits are not enforced for this user");
            return;
        }
        let err = HostWorldLock::acquire(dir.join(HostWorldLock::FILE_NAME), WorldLockMode::Shared)
            .unwrap_err();
        let HostWorldLockError::Io(source) = &err else {
            panic!("an unsearchable world is an I/O error, never an unguarded run: {err:?}");
        };
        assert_eq!(
            source.kind(),
            io::ErrorKind::PermissionDenied,
            "the directory refuses both the create and the read"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlinked_lock_file_is_refused() {
        // A link whose target is gone is still a link: the pre-check
        // reports it, and the open refuses it as well.
        let root = TempRoot::new("symlink-dangling");
        let path = root.path().join(HostWorldLock::FILE_NAME);
        std::os::unix::fs::symlink(root.path().join("missing"), &path).unwrap();
        let err = io_error(HostWorldLock::acquire(&path, WorldLockMode::Shared).unwrap_err());
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_lock_file_another_user_created_still_locks_exclusively() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = TempRoot::new("foreign");
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

        let root = TempRoot::new("read-only");
        let existing = root.path().join("existing");
        let fresh = root.path().join("fresh");
        let unreadable = root.path().join("unreadable");
        let probe = root.path().join("probe");
        for dir in [&existing, &fresh, &unreadable, &probe] {
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
        for dir in [&existing, &fresh, &unreadable, &probe] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o555)).unwrap();
        }
        let _restore = LockedDirs(vec![existing, unreadable, probe.clone(), fresh.clone()]);

        // Privileged users (and some filesystems) ignore permission bits;
        // nothing to observe then. The probe has its own directory, so
        // `fresh` stays a world without a lock file.
        if fs::write(probe.join("file"), "").is_err() {
            let err = HostWorldLock::acquire(&existing_lock, WorldLockMode::Shared).unwrap_err();
            assert!(
                matches!(err, HostWorldLockError::Contended),
                "a read-only handle still sees the holder"
            );
            drop(writer);
            let lock = HostWorldLock::acquire(&existing_lock, WorldLockMode::Shared).unwrap();
            assert!(
                lock.guard().is_ok(),
                "an existing lock file is still locked"
            );
            let lock =
                HostWorldLock::acquire(fresh.join(HostWorldLock::FILE_NAME), WorldLockMode::Shared)
                    .unwrap();
            // The reason stays reachable, so an unguarded run is never a
            // silent one.
            assert_eq!(
                lock.unguarded_reason(),
                Some(UnguardedReason::ReadOnlyWorld)
            );
            assert!(!lock.is_guarded());
            assert_eq!(
                lock.guard().unwrap_err(),
                UnguardedReason::ReadOnlyWorld,
                "no lock file degrades to unguarded"
            );
        } else {
            eprintln!("skipped: permission bits are not enforced for this user");
        }

        // The lock file this user cannot open at all needs its own mode
        // to be enforced, so it is checked on its own.
        if fs::File::open(&unreadable_lock).is_err() {
            let err = HostWorldLock::acquire(&unreadable_lock, WorldLockMode::Shared).unwrap_err();
            assert_eq!(
                io_error(err).kind(),
                io::ErrorKind::PermissionDenied,
                "a lock file this user cannot open never degrades"
            );
        } else {
            eprintln!("skipped: the unreadable lock file is still readable");
        }
    }
}
