use std::path::{Path, PathBuf};

use sekai_core::{Dimension, RegionKind};

use super::WorldTree;
use crate::discover::{self, Layout};
use crate::{RegionRef, WorldError};

/// A [`WorldTree`] rooted at a local directory.
///
/// [`HostWorldTree::new`] elects exactly one layout (Paper, legacy Bukkit,
/// vanilla 26.1, or legacy vanilla) and stores it; operations never
/// re-inspect the layout, and files from any non-elected layout stay
/// invisible. The root is stored as given, never canonicalized.
#[derive(Clone)]
pub struct HostWorldTree {
    root: PathBuf,
    layout: Layout,
}

impl HostWorldTree {
    /// Elect the layout of the tree rooted at `root`.
    ///
    /// A missing root resolves as an empty legacy tree and later discovery
    /// fails loudly; an unreadable root fails here.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, WorldError> {
        let root = root.as_ref();
        Ok(Self {
            root: root.to_path_buf(),
            layout: discover::resolve(root)?,
        })
    }

    /// Fresh output tree in the legacy single-folder layout (`region/`,
    /// `DIM-1/`, `DIM1/`), for roots that do not exist yet (export).
    pub fn new_legacy(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            layout: Layout::LegacyVanilla,
        }
    }

    /// Fresh output tree in the 26.1 layout
    /// (`dimensions/minecraft/<name>/`), for roots that do not exist yet
    /// (export).
    pub fn new_dimensions(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            layout: Layout::Vanilla,
        }
    }

    /// Fresh output tree in the Bukkit split-folder layout around `base`
    /// (`<base>/`, `<base>_nether/DIM-1/`, `<base>_the_end/DIM1/`), for
    /// roots that do not exist yet (export).
    pub fn new_bukkit(root: impl AsRef<Path>, base: impl Into<String>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            layout: Layout::LegacyPaper { base: base.into() },
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
        discover::discover(&self.root, &self.layout)
    }

    fn derive_path(
        &self,
        dim: Dimension,
        kind: RegionKind,
        region_x: i32,
        region_z: i32,
    ) -> Result<PathBuf, Self::Error> {
        self.layout
            .region_path(&self.root, dim, kind, region_x, region_z)
    }
}
