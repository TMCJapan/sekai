use std::path::{Path, PathBuf};

use sekai_core::{Dimension, RegionKind};

use super::WorldTree;
use crate::discover::{self, DimensionDirs};
use crate::{RegionRef, WorldError};

/// A [`WorldTree`] rooted at a local directory.
///
/// [`HostWorldTree::new`] classifies the root once and resolves every
/// derivable dimension directory: an elected Bukkit trio wins over a stray
/// `dimensions/` directory, otherwise an existing `dimensions/` directory
/// selects the 26.1 tree, else the legacy single-folder layout. The resolved
/// directories are stored, so operations never re-inspect the layout. The
/// root is stored as given, never canonicalized.
#[derive(Clone)]
pub struct HostWorldTree {
    root: PathBuf,
    dirs: DimensionDirs,
    /// Elected Bukkit overworld folder on container roots: discovery uses it
    /// so live server data wins over conversion leftovers.
    bukkit_base: Option<String>,
}

impl HostWorldTree {
    /// Classify the tree rooted at `root` and resolve its layout.
    ///
    /// A missing root resolves as an empty legacy tree and later discovery
    /// fails loudly; an unreadable root fails here.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, WorldError> {
        let (dirs, bukkit_base) = discover::resolve(&root)?;
        Ok(Self {
            root: root.as_ref().to_path_buf(),
            dirs,
            bukkit_base,
        })
    }

    /// Fresh output tree in the legacy single-folder layout (`region/`,
    /// `DIM-1/`, `DIM1/`), for roots that do not exist yet (export).
    pub fn new_legacy(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        Self {
            root: root.to_path_buf(),
            dirs: DimensionDirs::legacy(root),
            bukkit_base: None,
        }
    }

    /// Fresh output tree in the 26.1 layout
    /// (`dimensions/minecraft/<name>/`), for roots that do not exist yet
    /// (export).
    pub fn new_dimensions(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        Self {
            root: root.to_path_buf(),
            dirs: DimensionDirs::dimensions(root),
            bukkit_base: None,
        }
    }

    /// Fresh output tree in the Bukkit split-folder layout around `base`
    /// (`<base>/`, `<base>_nether/DIM-1/`, `<base>_the_end/DIM1/`), for
    /// roots that do not exist yet (export).
    pub fn new_bukkit(root: impl AsRef<Path>, base: impl Into<String>) -> Self {
        let root = root.as_ref();
        let base = base.into();
        Self {
            root: root.to_path_buf(),
            dirs: DimensionDirs::bukkit(root, &base),
            bukkit_base: Some(base),
        }
    }
}

impl WorldTree for HostWorldTree {
    type Error = WorldError;

    fn is_empty(&self) -> Result<bool, Self::Error> {
        match std::fs::read_dir(&self.root) {
            Ok(mut entries) => Ok(entries.next().is_none()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(source) => Err(WorldError::io(&self.root, source)),
        }
    }

    fn discover(&self) -> Result<Vec<RegionRef>, Self::Error> {
        discover::discover(&self.root, self.bukkit_base.as_deref())
    }

    fn derive_path(
        &self,
        dim: Dimension,
        kind: RegionKind,
        region_x: i32,
        region_z: i32,
    ) -> Result<PathBuf, Self::Error> {
        self.dirs.region_path(dim, kind, region_x, region_z)
    }
}
