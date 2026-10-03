use std::path::{Path, PathBuf};

use sekai_core::{Dimension, RegionKind};

use crate::{LayoutFlavor, RegionRef, WorldError};

/// A [`WorldTree`] rooted at a local directory.
///
/// Every operation resolves relative to the path passed to
/// [`HostWorktree::new`]; the path is stored as given, never canonicalized.
pub struct HostWorktree {
    path: PathBuf,
}

impl HostWorktree {
    /// Root a worktree at `path`.
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }
}

impl WorldTree for HostWorktree {
    fn is_empty(&self) -> Result<bool, std::io::Error> {
        match std::fs::read_dir(&self.path) {
            Ok(mut entries) => Ok(entries.next().is_none()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(source) => Err(std::io::Error::new(
                source.kind(),
                format!(
                    "failed to inspect export target {}: {source}",
                    self.path.display()
                ),
            )),
        }
    }

    fn discover(&self) -> Result<Vec<RegionRef>, WorldError> {
        crate::discover::discover(&self.path)
    }

    fn detect_flavor(&self) -> Result<LayoutFlavor, WorldError> {
        crate::discover::detect_flavor(&self.path)
    }

    fn derive_path(
        &self,
        flavor: &LayoutFlavor,
        dim: Dimension,
        kind: RegionKind,
        region_x: i32,
        region_z: i32,
    ) -> Result<PathBuf, WorldError> {
        crate::discover::derive_path(&self.path, flavor, dim, kind, region_x, region_z)
    }
}

impl AsRef<Path> for HostWorktree {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

/// World access shared by backup, export, rollback, and diff.
///
/// Bundles discovery, layout flavor detection, and derived paths so
/// `sekai-app` operations can accept any world implementation.
pub trait WorldTree: Sync + Send + 'static {
    fn is_empty(&self) -> Result<bool, std::io::Error>;
    fn discover(&self) -> Result<Vec<RegionRef>, WorldError>;
    fn detect_flavor(&self) -> Result<LayoutFlavor, WorldError>;
    fn derive_path(
        &self,
        flavor: &LayoutFlavor,
        dim: Dimension,
        kind: RegionKind,
        region_x: i32,
        region_z: i32,
    ) -> Result<PathBuf, WorldError>;
}
