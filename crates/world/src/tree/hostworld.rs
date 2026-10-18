//! Host-layout classification and dimension maps.
//!
//! Three server families share the `.mca` format but not the directory
//! layout: one vanilla world folder (`region/`, `DIM-1/`, `DIM1/`, or a
//! 26.1 `dimensions/minecraft/<name>/` tree), a pre-26.1 Bukkit server
//! root (`<base>/`, `<base>_nether/DIM-1/`, `<base>_the_end/DIM1/`, plus
//! one `<base>_<namespace>_<path>` sibling per datapack dimension, `base`
//! from `level-name`), or an arbitrary plugin world folder. `detect`
//! classifies a root once; each arm below collects the dimension
//! directories that layout uses.
//!
//! Every observed dimension is keyed by a hash of its canonical
//! `<namespace>:<name>` id (`minecraft:overworld`, `minecraft:the_nether`,
//! and `minecraft:the_end` for the trio), so ids stay stable when a world
//! moves between layouts. The trio's hashes are the `Dimension::OVERWORLD`/
//! `NETHER`/`END` constants, the ids every layout and the scoped operations
//! use.

mod bukkit;
mod detect;
mod vanilla;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sekai_core::{Dimension, RegionKind};

use super::WorldTree;
use crate::{RegionRef, WorldError};

/// Kind directories scanned for region files, in output order.
const KIND_DIRS: [(RegionKind, &str); 3] = [
    (RegionKind::REGION, "region"),
    (RegionKind::ENTITIES, "entities"),
    (RegionKind::POI, "poi"),
];

#[derive(Clone)]
pub struct HostWorldTree {
    /// Base path as passed by the caller. Dimension enumeration fails
    /// loudly when it is missing, while missing dimension/kind
    /// subdirectories are skipped.
    root: PathBuf,
    dirs: BTreeMap<Dimension, PathBuf>,
}

impl HostWorldTree {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, WorldError> {
        let root = root.as_ref();
        let dirs = match detect::detect(root)? {
            Some(detect::Layout::Bukkit(base)) => {
                collect_dimensions(root.join(base).join("dimensions"))?
            }
            Some(detect::Layout::BukkitLegacy(base)) => {
                let mut dirs = bukkit::collect_custom_dimensions(root, &base)?;
                dirs.extend(bukkit::fixed_dirs(root, &base));
                dirs
            }
            Some(detect::Layout::Vanilla) => collect_dimensions(root.join("dimensions"))?,
            Some(detect::Layout::VanillaLegacy) => {
                let mut dirs = collect_dimensions(root.join("dimensions"))?;
                dirs.extend([
                    (Dimension::OVERWORLD, root.to_path_buf()),
                    (Dimension::NETHER, root.join("DIM-1")),
                    (Dimension::END, root.join("DIM1")),
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

    /// Fail when the root is missing: resolution treats one as an empty
    /// legacy tree for fresh output, but dimension enumeration must not
    /// silently answer "nothing" for a world that vanished. Region
    /// enumeration stays a plain directory read instead, so output trees
    /// can serve a root that does not exist yet.
    fn check_root(&self) -> Result<(), std::io::Error> {
        if self.root.is_dir() {
            return Ok(());
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("world directory not found: {}", self.root.display()),
        ))
    }
}

/// Collect dimension directories, attaching the failing path to I/O errors.
fn collect_dimensions(dir: impl AsRef<Path>) -> Result<BTreeMap<Dimension, PathBuf>, WorldError> {
    let dir = dir.as_ref();
    vanilla::collect_dimensions(dir).map_err(|source| WorldError::io(dir, source))
}

impl WorldTree for HostWorldTree {
    type Error = std::io::Error;

    fn is_empty(&self) -> Result<bool, Self::Error> {
        for path in self.dirs.values() {
            match std::fs::read_dir(path) {
                Ok(mut entries) => {
                    if entries.next().is_some() {
                        return Ok(false);
                    }
                }
                // A directory that does not exist yet holds nothing.
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => return Err(source),
            }
        }
        Ok(true)
    }

    fn get_dims(&self) -> Result<Vec<Dimension>, Self::Error> {
        self.check_root()?;
        Ok(self.dirs.keys().copied().collect())
    }

    fn get_regions(&self, dim: Dimension) -> Result<Vec<RegionRef>, Self::Error> {
        let Some(dir) = self.dirs.get(&dim) else {
            return Ok(Vec::new());
        };

        let mut res = Vec::new();
        for (kind, kind_dir) in KIND_DIRS {
            let mut regions = BTreeMap::new();
            // Sorted by name so that two files spelling the same coordinates
            // (`r.0.0.mca` and `r.00.00.mca`, both of which the game itself
            // parses as region 0,0) resolve the same way on every run instead
            // of following directory order.
            let mut entries = vanilla::read_dir_opt(dir.join(kind_dir))?;
            entries.sort_unstable_by_key(std::fs::DirEntry::file_name);
            for entry in entries {
                if !vanilla::is_file(&entry) {
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
                    dim,
                    kind,
                    region_x,
                    region_z,
                };
                regions.entry((region_x, region_z)).or_insert(reference);
            }
            res.extend(regions.into_values());
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
        // Mirrors `WorldError::UnknownRegionPath`; the CLI troubleshooting
        // entry refers to this wording.
        let cannot_derive = || {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "cannot derive region path for dim {}, kind {}, r.{region_x}.{region_z}",
                    dim.raw(),
                    kind.raw()
                ),
            )
        };
        let root = self.dirs.get(&dim).ok_or_else(cannot_derive)?;

        let kind_dir = match kind {
            RegionKind::REGION => "region",
            RegionKind::ENTITIES => "entities",
            RegionKind::POI => "poi",
            _ => return Err(cannot_derive()),
        };

        Ok(root
            .join(kind_dir)
            .join(format!("r.{region_x}.{region_z}.mca")))
    }
}
