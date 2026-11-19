//! Where a command runs: the store and the worktree that `.sekai` names
//! (ADR-0035 *Identity and discovery*, *`init`*).
//!
//! `.sekai` sits at a world root. A directory there is the store, and its
//! world is the main worktree; a file there is a link file that names the
//! store and the worktree. Discovery walks up to the first `.sekai`, as
//! Git finds `.git`, and never falls back to matching paths.

use std::ffi::OsStr;
use std::fs::{self, Metadata};
use std::io;
use std::path::{Path, PathBuf};

use sekai_core::{
    DEFAULT_REF_NAME, Head, RefName, ReflogNote, Worktree, WorktreeName, find_worktree,
};
use sekai_storage_local::LocalStore;

use sekai_storage::MetaStore;

use crate::error::AppError;
use crate::instance::SekaiInstance;
use crate::time::now_ms;

/// The name of the store directory or link file at a world root.
pub use sekai_core::SEKAI_DIR;

/// The store a command opens and the world it runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// The store directory.
    pub store: PathBuf,
    /// The world the command runs in, or `None` outside every world.
    pub world: Option<WorldRoot>,
}

impl Location {
    /// The world the command runs in; fails outside every worktree with
    /// [`AppError::NoWorkTree`], as Git's commands that need a work tree
    /// do.
    pub fn world_tree(&self) -> Result<sekai_world_local::HostWorldTree, AppError> {
        let world = self.world.as_ref().ok_or(AppError::NoWorkTree)?;
        Ok(sekai_world_local::HostWorldTree::new(&world.root)?)
    }
}

/// A world root that discovery found, and which worktree it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldRoot {
    /// The directory that holds `.sekai`.
    pub root: PathBuf,
    /// The worktree a link file names, or `None` for the main worktree,
    /// whose name only the store records.
    pub linked: Option<WorktreeName>,
}

/// A link file: the store and the worktree of a world whose store lies
/// elsewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Link {
    pub(crate) store: PathBuf,
    pub(crate) worktree: WorktreeName,
}

impl Link {
    /// Read the link file at `path`: a `store:` line with an absolute path
    /// and a `worktree:` line with a worktree name.
    pub(crate) fn read(path: impl AsRef<Path>) -> Result<Self, AppError> {
        let path = path.as_ref();
        let text = fs::read_to_string(path).map_err(|source| io_at(path, &source))?;
        let bad = |reason| AppError::BadLinkFile {
            path: path.to_path_buf(),
            reason,
        };
        let (mut store, mut worktree) = (None, None);
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let (key, value) = line
                .split_once(':')
                .ok_or_else(|| bad("a line has no `:`"))?;
            let value = value.trim();
            let slot = match key.trim() {
                "store" => &mut store,
                "worktree" => &mut worktree,
                _ => return Err(bad("a line names an unknown key")),
            };
            if slot.replace(value).is_some() {
                return Err(bad("a key appears twice"));
            }
        }
        let store = PathBuf::from(store.ok_or_else(|| bad("no `store:` line"))?);
        if !store.is_absolute() {
            return Err(bad("the store path is not absolute"));
        }
        let worktree = WorktreeName::parse(worktree.ok_or_else(|| bad("no `worktree:` line"))?)
            .map_err(|_| bad("the worktree name is invalid"))?;
        Ok(Self { store, worktree })
    }

    pub(crate) fn write(&self, path: impl AsRef<Path>) -> Result<(), AppError> {
        let path = path.as_ref();
        let text = format!(
            "store: {}\nworktree: {}\n",
            utf8(&self.store)?,
            self.worktree
        );
        fs::write(path, text).map_err(|source| io_at(path, &source))
    }
}

/// What a `.sekai` at a world root is.
enum Sekai {
    Store,
    Link(Link),
}

/// Find the store and the world from `start`, walking up to the first
/// `.sekai` (ADR-0035 *Identity and discovery*). `store` is `--store`.
///
/// A link file gives the store and the worktree; `store` overrides only
/// the store. A store directory makes its world the main worktree, unless
/// `store` names another store, which leaves the command outside every
/// world. Without a `.sekai`, the command runs outside every world when
/// `store` is given, and fails otherwise.
///
/// The walk stops before it crosses into another filesystem, as Git's
/// does without `GIT_DISCOVERY_ACROSS_FILESYSTEM`. Only Unix reports one;
/// elsewhere the walk goes up to the root.
pub fn discover(
    start: impl AsRef<Path>,
    store: Option<impl AsRef<Path>>,
) -> Result<Location, AppError> {
    let start = start.as_ref();
    let store = store.map(|store| store.as_ref().to_path_buf());
    let start = start
        .canonicalize()
        .map_err(|source| io_at(start, &source))?;
    let outside = |store| Location { store, world: None };
    let Some((root, sekai)) = find_sekai(&start)? else {
        return store.map(outside).ok_or(AppError::NotInWorld { start });
    };
    let found = root.join(SEKAI_DIR);
    Ok(match (sekai, store) {
        (Sekai::Store, Some(store)) if !same_location(&store, &found) => outside(store),
        (Sekai::Store, store) => Location {
            store: store.unwrap_or(found),
            world: Some(WorldRoot { root, linked: None }),
        },
        (Sekai::Link(link), store) => Location {
            store: store.unwrap_or(link.store),
            world: Some(WorldRoot {
                root,
                linked: Some(link.worktree),
            }),
        },
    })
}

/// The first directory from `start` up that holds a `.sekai`, and what
/// that `.sekai` is.
fn find_sekai(start: impl AsRef<Path>) -> Result<Option<(PathBuf, Sekai)>, AppError> {
    let start = start.as_ref();
    let filesystem = device(&metadata(start)?);
    let mut anchored = true; // `start` itself was just statted
    for dir in start.ancestors() {
        if anchored {
            anchored = false;
        } else if device(&metadata(dir)?) != filesystem {
            return Ok(None);
        }
        let candidate = dir.join(SEKAI_DIR);
        let sekai = match fs::metadata(&candidate) {
            Ok(meta) if meta.is_dir() => Sekai::Store,
            Ok(_) => Sekai::Link(Link::read(&candidate)?),
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => return Err(io_at(&candidate, &source)),
        };
        return Ok(Some((dir.to_path_buf(), sekai)));
    }
    Ok(None)
}

fn metadata(path: impl AsRef<Path>) -> Result<Metadata, AppError> {
    let path = path.as_ref();
    fs::metadata(path).map_err(|source| io_at(path, &source))
}

/// The filesystem `meta` lives on; the same everywhere but on Unix, where
/// only `st_dev` tells filesystems apart.
#[cfg(unix)]
fn device(meta: &Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::dev(meta)
}

#[cfg(not(unix))]
const fn device(_: &Metadata) -> u64 {
    0
}

/// A worktree on an unborn `DEFAULT_REF_NAME`, with the note its
/// registration logs. HEAD unborn means no note is ever recorded.
fn unborn_worktree(
    name: WorktreeName,
    path: Option<String>,
) -> Result<(Worktree, ReflogNote<'static>), AppError> {
    Ok((
        Worktree {
            name,
            path,
            head: Head::Branch(RefName::parse(DEFAULT_REF_NAME)?),
        },
        ReflogNote::new("", now_ms()?),
    ))
}

/// Whether `a` and `b` name the same location: the same path, or the same
/// existing directory.
pub(crate) fn same_location(a: impl AsRef<Path>, b: impl AsRef<Path>) -> bool {
    let (a, b) = (a.as_ref(), b.as_ref());
    a == b
        || a.canonicalize()
            .is_ok_and(|a| b.canonicalize().is_ok_and(|b| a == b))
}

/// `path` made absolute, with symlinks resolved when it exists.
pub(crate) fn absolute(path: impl AsRef<Path>) -> Result<PathBuf, AppError> {
    let path = path.as_ref();
    path.canonicalize()
        .or_else(|_| std::path::absolute(path))
        .map_err(|source| io_at(path, &source))
}

pub(crate) fn utf8(path: impl AsRef<Path>) -> Result<String, AppError> {
    let path = path.as_ref();
    path.to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| AppError::NonUtf8Path {
            path: path.to_path_buf(),
        })
}

/// An I/O failure with the path it happened at.
pub(crate) fn io_at(path: impl AsRef<Path>, source: &io::Error) -> AppError {
    let path = path.as_ref();
    AppError::Io(io::Error::new(
        source.kind(),
        format!("{}: {source}", path.display()),
    ))
}

/// What `init` did.
pub struct Initialized {
    /// The store, opened on the world's worktree.
    pub instance: SekaiInstance<LocalStore>,
    /// The store directory.
    pub store: PathBuf,
    /// Whether the store was created; `false` for a reinitialization.
    pub created: bool,
}

impl SekaiInstance<LocalStore> {
    /// Open the store `location` names, acting on its worktree.
    ///
    /// A main worktree takes its name from the store, which fails with
    /// [`AppError::NoMainWorktree`] when the store has none. A linked one
    /// is taken as the link file names it; reading its HEAD fails if the
    /// store does not know it.
    pub async fn open_location(location: &Location) -> Result<Self, AppError> {
        let instance = Self::new(sekai_storage_local::open_local(&location.store).await?);
        let worktree = match &location.world {
            None => None,
            Some(WorldRoot {
                linked: Some(name), ..
            }) => Some(name.clone()),
            Some(WorldRoot { linked: None, .. }) => Some(
                find_worktree(instance.store.meta(), Worktree::is_main)
                    .await?
                    .map(|main| main.name)
                    .ok_or_else(|| AppError::NoMainWorktree {
                        store: location.store.clone(),
                    })?,
            ),
        };
        let mut instance = instance.with_worktree(worktree);
        instance.main_root = location.store.parent().map(Path::to_path_buf);
        Ok(instance)
    }

    /// Register `world` in a store, as `init [--store <store>] [--world
    /// <world>]` does (ADR-0035 *`init`*), leaving the world's files
    /// untouched.
    ///
    /// The store defaults to `<world>/.sekai`, whose world is the main
    /// worktree; a store elsewhere makes the world a linked worktree, and
    /// `init` writes the link file at `<world>/.sekai`. The worktree is
    /// named after the world's basename, and HEAD starts unborn on `main`.
    ///
    /// Initializing a world its store already has changes nothing. A
    /// world that is not recognizable fails with [`AppError::NotAWorld`],
    /// one whose `.sekai` names another store with
    /// [`AppError::WorldInOtherStore`], and an existing store that lacks
    /// the world with [`AppError::StoreHasOtherWorld`].
    pub async fn init_world(
        world: impl AsRef<Path>,
        store: Option<impl AsRef<Path>>,
    ) -> Result<Initialized, AppError> {
        let world = world.as_ref();
        let world = world
            .canonicalize()
            .map_err(|source| io_at(world, &source))?;
        if sekai_world_local::HostWorldTree::detect(&world)?.is_none() {
            return Err(AppError::NotAWorld { path: world });
        }
        let sekai = world.join(SEKAI_DIR);
        let store = store.map_or_else(|| Ok(sekai.clone()), absolute)?;
        let main = same_location(&store, &sekai);
        // A `.sekai` already at the world must name this store.
        let elsewhere = || AppError::WorldInOtherStore {
            world: world.clone(),
            sekai: sekai.clone(),
        };
        let link = match fs::metadata(&sekai) {
            Ok(meta) if meta.is_dir() => {
                if !main {
                    return Err(elsewhere());
                }
                None
            }
            Ok(_) => {
                let link = Link::read(&sekai)?;
                if !same_location(&link.store, &store) {
                    return Err(elsewhere());
                }
                Some(link)
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => None,
            Err(source) => return Err(io_at(&sekai, &source)),
        };

        let created = !sekai_storage_local::local_store_exists(&store);
        let mut instance = Self::new(sekai_storage_local::init_local(&store).await?);
        instance.main_root = store.parent().map(Path::to_path_buf);
        let path = if main { None } else { Some(utf8(&world)?) };
        let registered = find_worktree(instance.store.meta(), |worktree| worktree.path == path)
            .await?
            .map(|registered| registered.name);
        let name = match registered {
            Some(name) => name,
            None if created => {
                let basename = world.file_name().and_then(OsStr::to_str).ok_or_else(|| {
                    AppError::NonUtf8Path {
                        path: world.clone(),
                    }
                })?;
                let name = WorktreeName::sanitize(basename)?;
                let (worktree, note) = unborn_worktree(name, path)?;
                instance
                    .store
                    .meta_mut()
                    .add_worktree(&worktree, note)
                    .await?;
                worktree.name
            }
            None => return Err(AppError::StoreHasOtherWorld { store }),
        };
        if !main {
            let wanted = Link {
                store: store.clone(),
                worktree: name.clone(),
            };
            if link.as_ref() != Some(&wanted) {
                wanted.write(&sekai)?;
            }
        }
        Ok(Initialized {
            instance: instance.with_worktree(Some(name)),
            store,
            created,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("sekai-location-")
            .tempdir()
            .unwrap()
    }

    fn write(path: impl AsRef<Path>, text: &str) {
        let path = path.as_ref();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn a_link_file_round_trips_and_rejects_malformed_text() {
        let root = tempdir();
        let path = root.path().join(SEKAI_DIR);
        let link = Link {
            store: root.path().join("store"),
            worktree: WorktreeName::parse("w").unwrap(),
        };
        link.write(&path).unwrap();
        assert_eq!(Link::read(&path).unwrap(), link);

        // `/s` is not absolute on Windows, which wants a drive.
        let store = root.path().join("s");
        let store = store.to_str().unwrap();
        for (text, reason) in [
            (format!("store: {store}\n"), "no `worktree:` line"),
            ("worktree: w\n".into(), "no `store:` line"),
            (
                "store: s\nworktree: w\n".into(),
                "the store path is not absolute",
            ),
            (
                format!("store: {store}\nworktree: a..b\n"),
                "the worktree name is invalid",
            ),
            (
                format!("store: {store}\nstore: {store}\nworktree: w\n"),
                "a key appears twice",
            ),
            (
                format!("store: {store}\nworktree: w\nhead: x\n"),
                "a line names an unknown key",
            ),
            ("store /s\n".into(), "a line has no `:`"),
        ] {
            write(&path, &text);
            assert!(
                matches!(Link::read(&path), Err(AppError::BadLinkFile { reason: found, .. }) if found == reason),
                "{text:?}"
            );
        }
    }

    #[test]
    fn discovery_walks_up_to_the_first_sekai() {
        let root = tempdir();
        let world = root.path().canonicalize().unwrap().join("world");
        let store = world.join(SEKAI_DIR);
        fs::create_dir_all(store.join("blobs")).unwrap();
        fs::create_dir_all(world.join("region")).unwrap();

        let main = Location {
            store: store.clone(),
            world: Some(WorldRoot {
                root: world.clone(),
                linked: None,
            }),
        };
        assert_eq!(discover(world.join("region"), None::<&Path>).unwrap(), main);
        // `--store` naming the found store keeps the main worktree; any
        // other store leaves the command outside every world.
        assert_eq!(discover(&world, Some(&store)).unwrap(), main);
        let other = root.path().join("other");
        assert_eq!(
            discover(&world, Some(&other)).unwrap(),
            Location {
                store: other,
                world: None
            }
        );

        // The store's own directory is inside the world too.
        assert_eq!(discover(store.join("blobs"), None::<&Path>).unwrap(), main);
    }

    #[test]
    fn without_a_sekai_only_store_runs() {
        let root = tempdir();
        let start = root.path().canonicalize().unwrap();
        assert!(matches!(
            discover(&start, None::<&Path>),
            Err(AppError::NotInWorld { start: found }) if found == start
        ));
        let store = root.path().join("store");
        assert_eq!(
            discover(&start, Some(&store)).unwrap(),
            Location { store, world: None }
        );
    }

    #[test]
    fn a_link_file_names_the_worktree_and_store_overrides_only_the_store() {
        let root = tempdir();
        let world = root.path().canonicalize().unwrap().join("linked");
        let store = root.path().join("store");
        write(
            world.join(SEKAI_DIR),
            &format!("store: {}\nworktree: linked\n", store.display()),
        );
        let linked = Some(WorldRoot {
            root: world.clone(),
            linked: Some(WorktreeName::parse("linked").unwrap()),
        });
        assert_eq!(
            discover(&world, None::<&Path>).unwrap(),
            Location {
                store,
                world: linked.clone()
            }
        );
        let other = root.path().join("other");
        assert_eq!(
            discover(&world, Some(&other)).unwrap(),
            Location {
                store: other,
                world: linked
            }
        );
    }
}
