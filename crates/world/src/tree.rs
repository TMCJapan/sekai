use std::path::{Path, PathBuf};

use sekai_core::{Dimension, RegionKind};

use crate::{LayoutFlavor, RegionRef, WorldError};

impl WorldTree for PathBuf {
    fn is_empty(&self) -> Result<bool, std::io::Error> {
        WorldTree::is_empty(self.as_path())
    }
    fn discover(&self) -> Result<Vec<RegionRef>, WorldError> {
        WorldTree::discover(self.as_path())
    }
    fn detect_flavor(&self) -> Result<LayoutFlavor, WorldError> {
        WorldTree::detect_flavor(self.as_path())
    }
    fn derive_path(
        &self,
        flavor: &LayoutFlavor,
        dim: Dimension,
        kind: RegionKind,
        region_x: i32,
        region_z: i32,
    ) -> Result<PathBuf, WorldError> {
        WorldTree::derive_path(self.as_path(), flavor, dim, kind, region_x, region_z)
    }
}

impl WorldTree for Path {
    fn is_empty(&self) -> Result<bool, std::io::Error> {
        match std::fs::read_dir(self) {
            Ok(mut entries) => Ok(entries.next().is_none()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(source) => Err(std::io::Error::new(
                source.kind(),
                format!(
                    "failed to inspect export target {}: {source}",
                    self.display()
                ),
            )),
        }
    }
    fn discover(&self) -> Result<Vec<RegionRef>, WorldError> {
        crate::discover::discover(self)
    }
    fn detect_flavor(&self) -> Result<LayoutFlavor, WorldError> {
        crate::discover::detect_flavor(self)
    }
    fn derive_path(
        &self,
        flavor: &LayoutFlavor,
        dim: Dimension,
        kind: RegionKind,
        region_x: i32,
        region_z: i32,
    ) -> Result<PathBuf, WorldError> {
        crate::discover::derive_path(self, flavor, dim, kind, region_x, region_z)
    }
}

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
