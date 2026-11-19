//! Worktrees: `worktree add`, `worktree list`, and `worktree remove`
//! (ADR-0035 *`worktree add`*, *Identity and discovery*).
//!
//! `add` copies the source world's copy root to a new path, rebuilds every
//! region file there from a commit, and registers the copy as a linked
//! worktree. The store records no path for the main worktree, whose world
//! root is the directory that holds the store, so listing and removing
//! take the store's location.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sekai_core::find_worktree;
use sekai_core::{
    CheckoutReport, CommitId, Dimension, Head, HeadState, Ref, RefName, ReflogNote, RegionKey,
    RegionKind, Revision, Worktree, WorktreeName, WorktreeStart,
};
use sekai_storage::MetaStore;
use sekai_storage_local::LocalStore;
use sekai_world::{WorldLockMode, WorldTree};
use sekai_world_local::{HostWorldLock, HostWorldTree};

use crate::checkout::{CheckoutOptions, CheckoutTimings};
use crate::error::AppError;
use crate::error::{BranchUse, NameKind, Unremovable};
use crate::instance::{SekaiInstance, WorldGuard};
use crate::location::{Link, Location, SEKAI_DIR, absolute, io_at, same_location, utf8};
use crate::progress::BuildProgress;
use crate::refs::ref_err;
use crate::regions::discover_regions;
use crate::time::now_ms;

/// The game's own lock file in each world folder, which `worktree add`
/// leaves behind at any depth.
const SESSION_LOCK: &str = "session.lock";

/// The directory `worktree add` copies (ADR-0035 *`worktree add`*): the
/// server directory around the world when there is one, and the world
/// root otherwise.
///
/// A Bukkit root is the server directory. A vanilla world's parent is a
/// server directory when it holds a `server.properties` whose
/// `level-name` names the world, `world` when the key is absent as for
/// the game. One whose `level-name` names another directory leaves the
/// world on its own, and one that cannot be read or has only invalid
/// `level-name` values fails.
fn copy_root(tree: &HostWorldTree) -> Result<PathBuf, AppError> {
    use sekai_world_local::{LayoutFamily, LevelName, SERVER_PROPERTIES};

    let root = tree.root();
    if tree.family() == LayoutFamily::Bukkit {
        return Ok(root.to_path_buf());
    }
    let Some(parent) = root.parent() else {
        return Ok(root.to_path_buf());
    };
    let level_name = match sekai_world_local::read_level_name(parent)? {
        LevelName::Missing => return Ok(root.to_path_buf()),
        LevelName::Absent => "world".to_owned(),
        LevelName::Named(name) => name,
        LevelName::Invalid => {
            return Err(sekai_world_local::WorldError::InvalidLevelName {
                path: parent.join(SERVER_PROPERTIES),
            }
            .into());
        }
    };
    Ok(
        if root.file_name() == Some(std::ffi::OsStr::new(&level_name)) {
            parent.to_path_buf()
        } else {
            root.to_path_buf()
        },
    )
}

/// What `worktree add` created.
#[derive(Debug, Clone)]
pub struct AddedWorktree {
    /// Name of the new worktree.
    pub name: WorktreeName,
    /// World root of the new worktree, which holds its link file.
    pub root: PathBuf,
    /// The new worktree's HEAD.
    pub head: Head,
    /// Commit the new worktree is checked out at.
    pub commit: CommitId,
    /// Whether HEAD's branch was created for the new worktree.
    pub created_branch: bool,
    /// What rebuilding the region files wrote.
    pub report: CheckoutReport,
    /// Per-phase timings of that rebuild.
    pub timings: CheckoutTimings,
}

/// A worktree in a listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    /// Worktree name.
    pub name: WorktreeName,
    /// Absolute world root: the recorded one for a linked worktree, and
    /// the directory that holds the store for the main worktree.
    pub path: PathBuf,
    /// Whether this is the main worktree.
    pub main: bool,
    /// HEAD resolved against the refs.
    pub head: HeadState,
}

impl SekaiInstance<LocalStore> {
    /// Add a linked worktree at `path`, copied from the worktree that
    /// `location` names, as `worktree add <path> [<commit-ish>]` does
    /// (ADR-0035 *`worktree add`*).
    ///
    /// `path` must be empty or missing, and takes the place of the
    /// source's copy root, with the world
    /// root at the same position under it. Everything under the copy root
    /// is copied except the source's region files, the store, every
    /// `.sekai`, every `session.lock` and sekai lock file, and FIFOs,
    /// sockets, and devices; the region files are then rebuilt from the
    /// commit in the source's layout. A region directory that a copied
    /// symlink leads out of `path` fails with
    /// [`AppError::RegionDirOutsideTarget`] before any is written. A
    /// failure or a cancellation removes `path` once every write to it
    /// has ended, including an empty `path` that already existed.
    ///
    /// `commit_ish` decides HEAD as [`sekai_core::resolve_worktree_head`]
    /// does, with the basename of `path` as the branch when it is `None`;
    /// a basename that is not a valid branch name fails with
    /// [`AppError::InvalidName`]. The worktree is named after the
    /// sanitized basename, with a numeric suffix on a clash.
    pub async fn add_worktree(
        &mut self,
        location: &Location,
        path: impl AsRef<Path>,
        commit_ish: Option<&Revision>,
        progress_builder: &impl BuildProgress,
    ) -> Result<AddedWorktree, AppError> {
        let world = location.world.as_ref().ok_or(AppError::NoWorkTree)?;
        let source = self.worktree.as_ref().ok_or(AppError::NoWorkTree)?;
        let path = path.as_ref();
        // Errors name the target as given, as Git's do.
        check_target(path)?;
        let target = std::path::absolute(path).map_err(|source| io_at(path, &source))?;
        let basename =
            target
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or_else(|| AppError::NonUtf8Path {
                    path: target.clone(),
                })?;

        let meta = self.store.meta();
        let branch;
        let start = if let Some(revision) = commit_ish {
            WorktreeStart::CommitIsh(revision)
        } else {
            branch = RefName::parse(basename).map_err(|_| AppError::InvalidName {
                kind: NameKind::Branch,
                name: basename.to_owned(),
            })?;
            WorktreeStart::Basename(&branch)
        };
        let new = sekai_core::resolve_worktree_head(meta, Some(source), start).await?;
        if let Head::Branch(name) = &new.head {
            self.refuse_checked_out(name, None, BranchUse::CheckOut)
                .await?;
        }
        let name =
            sekai_core::unique_worktree_name(meta, &WorktreeName::sanitize(basename)?).await?;

        let source_tree = HostWorldTree::new(&world.root)?;
        let plan = sekai_core::usecase::checkout::plan_checkout(meta, new.commit)
            .await
            .map_err(AppError::Checkout)?;
        let resolved: BTreeSet<Dimension> = source_tree.get_dims()?.into_iter().collect();
        if let Some(key) = plan.groups.keys().find(|key| !resolved.contains(&key.dim)) {
            return Err(AppError::UnresolvedDimension { dim: key.dim });
        }
        let copy_root = copy_root(&source_tree)?;
        let world_in_copy = world.root.strip_prefix(&copy_root).map_err(|_| {
            io_at(
                &world.root,
                &io::Error::other("the world root lies outside its copy root"),
            )
        })?;
        let store = absolute(&location.store)?;

        fs::create_dir_all(&target).map_err(|source| io_at(&target, &source))?;
        let cleanup = Arc::new(RemoveOnDrop::new(target.clone()));
        let target = target
            .canonicalize()
            .map_err(|source| io_at(&target, &source))?;
        // Collecting the components drops the separator that joining an
        // empty `world_in_copy` would leave at the end.
        let root: PathBuf = target.join(world_in_copy).components().collect();
        let recorded = utf8(&root)?;

        let meta = self.store.meta();
        if let Some(worktree) =
            find_worktree(meta, |worktree| worktree.path.as_ref() == Some(&recorded)).await?
        {
            return Err(AppError::PathRegistered {
                path: path.to_path_buf(),
                worktree: worktree.name,
            });
        }

        // Every write to the target, the copy included, holds this lock,
        // and the target goes only once the last of them has ended.
        fs::create_dir_all(&root).map_err(|source| io_at(&root, &source))?;
        let mut target_tree = source_tree.rebased(&root)?;
        let target_guard =
            WorldGuard::acquire_then(&target_tree, WorldLockMode::Exclusive, Arc::clone(&cleanup))?;
        let source_guard = WorldGuard::acquire(&source_tree, WorldLockMode::Shared)?;
        let skip = BTreeSet::from([store.clone(), target.clone()]);
        let (tree, from, to) = (source_tree.clone(), copy_root.clone(), target.clone());
        let copy_guard = Arc::clone(&target_guard);
        tokio::task::spawn_blocking(move || {
            let _guards = (source_guard, copy_guard);
            let mut skip = skip;
            skip.extend(
                discover_regions(&tree)?
                    .into_values()
                    .map(|region| region.as_path().to_path_buf()),
            );
            copy_dir(&from, &to, &skip, &[])
        })
        .await??;
        check_region_dirs(&target_tree, &resolved, &target)?;

        let (report, timings) = self
            .world_mut(&mut target_tree)
            .restore_planned(
                plan,
                CheckoutOptions::default(),
                &target_guard,
                progress_builder,
            )
            .await?;
        Link {
            store,
            worktree: name.clone(),
        }
        .write(root.join(SEKAI_DIR))?;

        let worktree = Worktree {
            name,
            path: Some(recorded),
            head: new.head,
        };
        self.register_worktree(&worktree, new.creates_branch, new.commit)
            .await?;
        cleanup.disarm();

        Ok(AddedWorktree {
            name: worktree.name,
            root,
            head: worktree.head,
            commit: new.commit,
            created_branch: new.creates_branch,
            report,
            timings,
        })
    }

    /// Register `worktree`, first creating its HEAD's branch at `commit`
    /// when `creates_branch` is set. The store writes the two apart, so a
    /// failed registration deletes the branch again rather than leave it
    /// checked out nowhere.
    async fn register_worktree(
        &mut self,
        worktree: &Worktree,
        creates_branch: bool,
        commit: CommitId,
    ) -> Result<(), AppError> {
        let at_ms = now_ms()?;
        let meta = self.store.meta_mut();
        let created = match &worktree.head {
            Head::Branch(branch) if creates_branch => {
                let branch = Ref::branch(branch.clone());
                let note = ReflogNote::new("branch: Created from HEAD", at_ms);
                meta.create_ref(&branch, commit, note)
                    .await
                    .map_err(ref_err(BranchUse::CheckOut))?;
                Some(branch)
            }
            _ => None,
        };
        // Git's first HEAD log entry for a new worktree has no message.
        let registered = meta
            .add_worktree(worktree, ReflogNote::new("", at_ms))
            .await
            .map_err(ref_err(BranchUse::CheckOut));
        if registered.is_err()
            && let Some(branch) = created
        {
            // Best effort: the registration failure is the one to report.
            let _ = meta.delete_ref(&branch).await;
        }
        registered
    }

    /// Every worktree of the store at `store`, as `worktree list` shows
    /// them: the main worktree first, then the linked ones by path, as Git
    /// orders them.
    pub async fn list_worktrees(
        &self,
        store: impl AsRef<Path>,
    ) -> Result<Vec<WorktreeEntry>, AppError> {
        let store = absolute(store)?;
        let meta = self.store.meta();
        let mut entries = Vec::new();
        for (path, worktree) in self.worktree_roots(&store).await? {
            let (_, head) = sekai_core::head_state(meta, Some(&worktree.name)).await?;
            entries.push(WorktreeEntry {
                main: worktree.is_main(),
                path,
                name: worktree.name,
                head,
            });
        }
        entries.sort_by(|a, b| b.main.cmp(&a.main).then_with(|| a.path.cmp(&b.path)));
        Ok(entries)
    }

    /// Remove the worktree `worktree` names from the store at `store`, as
    /// `worktree remove <worktree>` does (ADR-0035 *Identity and
    /// discovery*). Returns the removed worktree's record.
    ///
    /// `worktree` names a worktree by trailing path components unique
    /// among the worktrees, or else by its path, as Git's
    /// `find_worktree` does. The row goes with its HEAD log, and so does
    /// the link file when it still names the worktree; the world's files
    /// stay, and a world already gone is no obstacle. The main worktree
    /// and a match for nothing fail with
    /// [`AppError::UnremovableWorktree`].
    pub async fn remove_worktree(
        &mut self,
        store: impl AsRef<Path>,
        worktree: &str,
    ) -> Result<WorktreeEntry, AppError> {
        let store = absolute(store)?;
        let roots = self.worktree_roots(&store).await?;
        let refuse = |reason| AppError::UnremovableWorktree {
            reason,
            worktree: worktree.to_owned(),
        };
        let (path, found) =
            find_root(&roots, worktree)?.ok_or_else(|| refuse(Unremovable::NoMatch))?;
        if found.is_main() {
            return Err(refuse(Unremovable::Main));
        }
        let (_, head) = sekai_core::head_state(self.store.meta(), Some(&found.name)).await?;
        let removed = WorktreeEntry {
            name: found.name.clone(),
            path: path.clone(),
            main: false,
            head,
        };
        remove_link(path.join(SEKAI_DIR), &store, &found.name)?;
        self.store.meta_mut().remove_worktree(&found.name).await?;
        Ok(removed)
    }

    /// Every worktree row of the store at `store` with its absolute world
    /// root: the recorded one for a linked worktree, and the directory
    /// that holds the store for the main worktree. The caller absolutes
    /// `store`.
    async fn worktree_roots(
        &self,
        store: impl AsRef<Path>,
    ) -> Result<Vec<(PathBuf, Worktree)>, AppError> {
        let store = store.as_ref();
        let main_root = store.parent().unwrap_or(store);
        let mut roots = Vec::new();
        self.store
            .meta()
            .visit_worktrees(|worktree| {
                let path = worktree
                    .path
                    .as_ref()
                    .map_or_else(|| main_root.to_path_buf(), PathBuf::from);
                roots.push((path, worktree.clone()));
                true
            })
            .await?;
        Ok(roots)
    }
}

/// Fail unless `target` is missing or an empty directory.
fn check_target(target: impl AsRef<Path>) -> Result<(), AppError> {
    let target = target.as_ref();
    let exists = || AppError::TargetExists {
        path: target.to_path_buf(),
    };
    match fs::symlink_metadata(target) {
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_at(target, &source)),
        Ok(meta) if !meta.is_dir() => Err(exists()),
        Ok(_) => {
            let mut entries = fs::read_dir(target).map_err(|source| io_at(target, &source))?;
            entries.next().map_or(Ok(()), |_| Err(exists()))
        }
    }
}

/// The worktree `arg` names among `roots`: by trailing path components
/// unique among them, or else by path.
fn find_root<'a>(
    roots: &'a [(PathBuf, Worktree)],
    arg: &str,
) -> Result<Option<&'a (PathBuf, Worktree)>, AppError> {
    if arg.is_empty() {
        return Ok(None);
    }
    let mut by_suffix = roots.iter().filter(|(path, _)| path.ends_with(arg));
    if let (Some(found), None) = (by_suffix.next(), by_suffix.next()) {
        return Ok(Some(found));
    }
    let arg = absolute(arg)?;
    Ok(roots.iter().find(|(path, _)| same_location(path, &arg)))
}

/// Remove the link file at `path` when it names worktree `name` of the
/// store at `store`; anything else there belongs to someone else.
fn remove_link(
    path: impl AsRef<Path>,
    store: impl AsRef<Path>,
    name: &WorktreeName,
) -> Result<(), AppError> {
    let (path, store) = (path.as_ref(), store.as_ref());
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io_at(path, &source)),
    }
    let ours = Link::read(path)
        .is_ok_and(|link| link.worktree == *name && same_location(&link.store, store));
    if ours {
        fs::remove_file(path).map_err(|source| io_at(path, &source))?;
    }
    Ok(())
}

/// Fail unless every directory `tree` keeps region files in for `dims`
/// resolves inside `target`. A symlink copied from the source can lead
/// there, and the rebuild would then overwrite and delete the source
/// world's own region files.
fn check_region_dirs(
    tree: &HostWorldTree,
    dims: &BTreeSet<Dimension>,
    target: impl AsRef<Path>,
) -> Result<(), AppError> {
    let target = target.as_ref();
    for &dim in dims {
        for kind in RegionKind::ALL {
            let region = tree.derive_region(RegionKey::new(dim, kind, 0, 0))?;
            let Some(dir) = region.as_path().parent() else {
                continue;
            };
            // The directory itself need not exist yet: its nearest
            // existing ancestor decides where it would be created.
            let resolved = dir.ancestors().find_map(|dir| dir.canonicalize().ok());
            if !resolved.is_some_and(|resolved| resolved.starts_with(target)) {
                return Err(AppError::RegionDirOutsideTarget {
                    path: dir.to_path_buf(),
                });
            }
        }
    }
    Ok(())
}

/// Copy the contents of `from` into the existing directory `to`, leaving
/// out every path in `skip`, every `.sekai` (stores and the link files of
/// nested worktrees), every lock file, and everything but directories,
/// regular files, and symlinks: FIFOs, sockets, and devices hold no data
/// to copy, and opening a FIFO would wait for a writer.
///
/// `following` lists the directories reached through a symlink that are
/// being copied, on platforms where symlinks are followed.
fn copy_dir(
    from: impl AsRef<Path>,
    to: impl AsRef<Path>,
    skip: &BTreeSet<PathBuf>,
    following: &[PathBuf],
) -> Result<(), AppError> {
    let (from, to) = (from.as_ref(), to.as_ref());
    for entry in fs::read_dir(from).map_err(|source| io_at(from, &source))? {
        let entry = entry.map_err(|source| io_at(from, &source))?;
        let source = entry.path();
        let name = entry.file_name();
        if skip.contains(&source)
            || name == SEKAI_DIR
            || name == SESSION_LOCK
            || name == HostWorldLock::FILE_NAME
        {
            continue;
        }
        let dest = to.join(&name);
        let file_type = entry.file_type().map_err(|err| io_at(&source, &err))?;
        if file_type.is_symlink() {
            copy_symlink(&source, &dest, skip, following)?;
        } else if file_type.is_dir() {
            create_dir(&dest)?;
            copy_dir(&source, &dest, skip, following)?;
        } else if file_type.is_file() {
            fs::copy(&source, &dest).map_err(|err| io_at(&source, &err))?;
        }
    }
    Ok(())
}

/// Create the directory `dest`, which may already exist: the target's
/// world root is created before the copy, to hold its lock.
fn create_dir(dest: impl AsRef<Path>) -> Result<(), AppError> {
    let dest = dest.as_ref();
    match fs::create_dir(dest) {
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists && dest.is_dir() => Ok(()),
        created => created.map_err(|err| io_at(dest, &err)),
    }
}

/// Recreate the symlink `source` at `dest`, pointing where it points.
#[cfg(unix)]
fn copy_symlink(
    source: impl AsRef<Path>,
    dest: impl AsRef<Path>,
    _: &BTreeSet<PathBuf>,
    _: &[PathBuf],
) -> Result<(), AppError> {
    let (source, dest) = (source.as_ref(), dest.as_ref());
    let points_to = fs::read_link(source).map_err(|err| io_at(source, &err))?;
    std::os::unix::fs::symlink(points_to, dest).map_err(|err| io_at(dest, &err))
}

/// Copy what the symlink `source` points to, where links need privileges
/// to create.
///
/// The copy goes on from the resolved path, so `skip` (which holds
/// resolved paths) still matches beneath it. A link back into a
/// directory being copied, its own included, fails instead of copying
/// forever.
#[cfg(not(unix))]
fn copy_symlink(
    source: impl AsRef<Path>,
    dest: impl AsRef<Path>,
    skip: &BTreeSet<PathBuf>,
    following: &[PathBuf],
) -> Result<(), AppError> {
    let (source, dest) = (source.as_ref(), dest.as_ref());
    let resolved = source.canonicalize().map_err(|err| io_at(source, &err))?;
    if skip.contains(&resolved) {
        return Ok(());
    }
    let meta = fs::metadata(&resolved).map_err(|err| io_at(&resolved, &err))?;
    if meta.is_file() {
        return fs::copy(&resolved, dest)
            .map(drop)
            .map_err(|err| io_at(source, &err));
    }
    if !meta.is_dir() {
        return Ok(());
    }
    let parent = source
        .parent()
        .map(Path::canonicalize)
        .transpose()
        .map_err(|err| io_at(source, &err))?;
    let loops = parent
        .iter()
        .chain(following)
        .any(|dir| dir.starts_with(&resolved));
    if loops {
        return Err(io_at(
            source,
            &io::Error::other("the symlink leads back into a directory being copied"),
        ));
    }
    create_dir(dest)?;
    let mut following = following.to_vec();
    following.push(resolved.clone());
    copy_dir(&resolved, dest, skip, &following)
}

/// Removes a directory tree on drop, unless disarmed: a failed or
/// cancelled `worktree add` leaves no target behind, as Git's does.
/// Even an empty target that existed before is removed. Shared with the
/// target's lock, it drops only after every write to the target has ended.
struct RemoveOnDrop {
    path: PathBuf,
    armed: AtomicBool,
}

impl RemoveOnDrop {
    const fn new(path: PathBuf) -> Self {
        Self {
            path,
            armed: AtomicBool::new(true),
        }
    }

    fn disarm(&self) {
        self.armed.store(false, Ordering::Relaxed);
    }
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if *self.armed.get_mut() {
            // Best effort: the failure that got here is the one to report.
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_chunk_image() -> Vec<u8> {
        let mut builder = sekai_anvil::RegionBuilder::new(0, 0, 0).unwrap();
        builder.stage_chunk(0, 0, &[2, 1, 2, 3]).unwrap();
        builder.image().unwrap()
    }

    fn write(path: impl AsRef<Path>, bytes: &[u8]) {
        std::fs::create_dir_all(path.as_ref().parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn the_copy_root_is_the_server_directory_around_a_world() {
        let root_path = tempfile::tempdir().unwrap();
        let image = one_chunk_image();

        // A Bukkit root is the server directory itself.
        let bukkit = root_path.path().join("bukkit");
        write(bukkit.join("server.properties"), b"level-name=world\n");
        write(bukkit.join("world/region/r.0.0.mca"), &image);
        assert_eq!(
            copy_root(&HostWorldTree::new(&bukkit).unwrap()).unwrap(),
            bukkit
        );

        // A vanilla world alone, as a single-player save is.
        let save = root_path.path().join("saves/one");
        write(save.join("region/r.0.0.mca"), &image);
        assert_eq!(
            copy_root(&HostWorldTree::new(&save).unwrap()).unwrap(),
            save
        );

        // A vanilla world inside a server directory, named or not by its
        // `level-name`.
        let server = root_path.path().join("server");
        let world = server.join("world");
        write(world.join("region/r.0.0.mca"), &image);
        write(
            server.join("server.properties"),
            b"motd=x\nlevel-name=world\n",
        );
        assert_eq!(
            copy_root(&HostWorldTree::new(&world).unwrap()).unwrap(),
            server
        );
        write(server.join("server.properties"), b"level-name=other\n");
        assert_eq!(
            copy_root(&HostWorldTree::new(&world).unwrap()).unwrap(),
            world
        );

        // Spaces around `=` are allowed, and an absent `level-name` means
        // `world`, as for the game.
        for text in [&b"level-name = world\n"[..], b"motd=x\n"] {
            write(server.join("server.properties"), text);
            assert_eq!(
                copy_root(&HostWorldTree::new(&world).unwrap()).unwrap(),
                server
            );
        }

        // Only invalid `level-name` values fail.
        write(server.join("server.properties"), b"level-name=../escape\n");
        assert!(matches!(
            copy_root(&HostWorldTree::new(&world).unwrap()),
            Err(AppError::World(
                sekai_world_local::WorldError::InvalidLevelName { path }
            )) if path == server.join("server.properties")
        ));
    }

    fn root(name: &str, path: &str, main: bool) -> (PathBuf, Worktree) {
        let worktree = Worktree {
            name: WorktreeName::parse(name).unwrap(),
            path: (!main).then(|| path.to_owned()),
            head: Head::Branch(RefName::parse(name).unwrap()),
        };
        (PathBuf::from(path), worktree)
    }

    #[test]
    fn a_worktree_is_found_by_unique_trailing_components() {
        let roots = [
            root("main", "/srv/main", true),
            root("a", "/x/one/wt", false),
            root("b", "/x/two/wt", false),
            root("c", "/x/mywt", false),
        ];
        let name = |arg| {
            find_root(&roots, arg)
                .unwrap()
                .map(|(_, worktree)| worktree.name.to_string())
        };
        assert_eq!(name("one/wt").as_deref(), Some("a"));
        assert_eq!(name("mywt").as_deref(), Some("c"));
        assert_eq!(name("main").as_deref(), Some("main"));
        // An ambiguous suffix falls through to the path, as in Git.
        assert_eq!(name("wt"), None);
        assert_eq!(name("/x/two/wt").as_deref(), Some("b"));
        // Components match whole: `ywt` is no component of `mywt`.
        assert_eq!(name("ywt"), None);
        assert_eq!(name(""), None);
    }
}
