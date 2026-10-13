use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sekai_core::{Dimension, RegionKind};

use super::WorldTree;
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
    dirs: BTreeMap<Dimension, PathBuf>,
}

impl HostWorldTree {
    /// Classify the tree rooted at `root` and resolve its layout.
    ///
    /// A missing root resolves as an empty legacy tree and later discovery
    /// fails loudly; an unreadable root fails here.
    pub fn new(_root: impl AsRef<Path>) -> Result<Self, WorldError> {
        // TODO: detect world layout and collect.
        Ok(Self {
            dirs: BTreeMap::new(),
        })
    }

    /// Fresh output tree in the legacy single-folder layout (`region/`,
    /// `DIM-1/`, `DIM1/`), for roots that do not exist yet (export).
    pub fn new_legacy(_root: impl AsRef<Path>) -> Self {
        // TODO
        Self {
            dirs: BTreeMap::new(),
        }
    }

    /// Fresh output tree in the 26.1 layout
    /// (`dimensions/minecraft/<name>/`), for roots that do not exist yet
    /// (export).
    pub fn new_dimensions(_root: impl AsRef<Path>) -> Self {
        // TODO
        Self {
            dirs: BTreeMap::new(),
        }
    }

    /// Fresh output tree in the Bukkit split-folder layout around `base`
    /// (`<base>/`, `<base>_nether/DIM-1/`, `<base>_the_end/DIM1/`), for
    /// roots that do not exist yet (export).
    pub fn new_bukkit(_root: impl AsRef<Path>, _base: impl Into<String>) -> Self {
        // TODO
        Self {
            dirs: BTreeMap::new(),
        }
    }
}

impl WorldTree for HostWorldTree {
    type Error = std::io::Error;

    fn is_empty(&self) -> Result<bool, Self::Error> {
        for path in self.dirs.values() {
            return match std::fs::read_dir(path).map(|mut entries| entries.next().is_some()) {
                Ok(true) => Ok(false),
                Err(source) if source.kind() != std::io::ErrorKind::NotFound => Err(source),
                _ => continue,
            };
        }
        Ok(true)
    }

    fn discover(&self) -> Result<Vec<RegionRef>, Self::Error> {
        const KIND_DIRS: [(RegionKind, &str); 3] = [
            (RegionKind::REGION, "region"),
            (RegionKind::ENTITIES, "entities"),
            (RegionKind::POI, "poi"),
        ];

        let mut res = Vec::new();
        for (dim, dir) in &self.dirs {
            for (kind, kind_dir) in KIND_DIRS {
                // Sorted by name so that two files spelling the same coordinates
                // (`r.0.0.mca` and `r.00.00.mca`, both of which the game itself
                // parses as region 0,0) resolve the same way on every run instead
                // of following directory order.
                let path = dir.join(kind_dir);
                let entries = std::fs::read_dir(&path)?.collect::<Result<Vec<_>, _>>()?;
                for entry in entries {
                    if !std::fs::metadata(entry.path())?.is_file() {
                        continue;
                    }
                    let file_name = entry.file_name();
                    let Some(name) = file_name.to_str() else {
                        continue;
                    };
                    let Ok((region_x, region_z)) = sekai_anvil::parse_region_name(name) else {
                        continue;
                    };
                    let reference = RegionRef {
                        path: entry.path(),
                        dim: *dim,
                        kind,
                        region_x,
                        region_z,
                    };
                    res.push(reference);
                }
            }
        }
        Ok(res)
    }

    fn derive_path(
        &self,
        dim: Dimension,
        kind: RegionKind,
        region_x: i32,
        region_z: i32,
    ) -> Result<PathBuf, Self::Error> {
        let root = self.dirs.get(&dim).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "unknown dimension")
        })?;

        let kind_dir = match kind {
            RegionKind::REGION => "region",
            RegionKind::ENTITIES => "entities",
            RegionKind::POI => "poi",
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "unknown region path",
            ))?,
        };

        Ok(root
            .join(kind_dir)
            .join(format!("r.{region_x}.{region_z}.mca")))
    }
}
