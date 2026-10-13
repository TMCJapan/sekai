mod detect;
mod vanilla;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sekai_anvil::custom_dimension_id;
use sekai_core::{Dimension, RegionKind};

use super::WorldTree;
use crate::{RegionRef, WorldError};

#[derive(Clone)]
pub struct HostWorldTree {
    /// Base path as passed by the caller. Discovery fails loudly when it is
    /// missing, while missing dimension/kind subdirectories are skipped.
    root: PathBuf,
    dirs: BTreeMap<Dimension, PathBuf>,
}

impl HostWorldTree {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, WorldError> {
        let root = root.as_ref();
        let dirs = match detect::detect(root) {
            Some(detect::Layout::Bukkit(base)) => {
                vanilla::collect_dimentions(root.join(base)).unwrap()
            }
            Some(detect::Layout::BukkitLegacy(base)) => {
                panic!("legacy bukkit layout is not supported. {base:?}")
            }
            Some(detect::Layout::Vanilla) => vanilla::collect_dimentions(root).unwrap(),
            Some(detect::Layout::VanillaLegacy) => {
                let mut dirs = vanilla::collect_dimentions(root).unwrap();
                dirs.extend([
                    (
                        custom_dimension_id("minecraft:overworld"),
                        root.to_path_buf(),
                    ),
                    (custom_dimension_id("minecraft:nether"), root.join("DIM-1")),
                    (custom_dimension_id("minecraft:the_end"), root.join("DIM1")),
                ]);
                dirs
            }
            None => return Ok(Self::new_legacy(root)),
        };
        Ok(Self {
            root: root.to_path_buf(),
            dirs,
        })
    }

    /// Fresh output tree in the legacy single-folder layout (`region/`,
    /// `DIM-1/`, `DIM1/`), for roots that do not exist yet (export).
    pub fn new_legacy(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        Self {
            root: root.to_path_buf(),
            dirs: [
                (Dimension::OVERWORLD, root.to_path_buf()),
                (Dimension::NETHER, root.join("DIM-1")),
                (Dimension::END, root.join("DIM1")),
            ]
            .into(),
        }
    }

    /// Fresh output tree in the 26.1 layout
    /// (`dimensions/minecraft/<name>/`), for roots that do not exist yet
    /// (export).
    pub fn new_dimensions(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        let minecraft = root.join("dimensions").join("minecraft");
        Self {
            root: root.to_path_buf(),
            dirs: [
                (Dimension::OVERWORLD, minecraft.join("overworld")),
                (Dimension::NETHER, minecraft.join("the_nether")),
                (Dimension::END, minecraft.join("the_end")),
            ]
            .into(),
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
            dirs: [
                (Dimension::OVERWORLD, root.join(&base)),
                (
                    Dimension::NETHER,
                    root.join(format!("{base}_nether")).join("DIM-1"),
                ),
                (
                    Dimension::END,
                    root.join(format!("{base}_the_end")).join("DIM1"),
                ),
            ]
            .into(),
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
        if !self.root.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("world directory not found: {}", self.root.display()),
            ));
        }

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
                let entries = match std::fs::read_dir(&path) {
                    Ok(entries) => entries.collect::<Result<Vec<_>, _>>()?,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(err) => return Err(err),
                };
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
