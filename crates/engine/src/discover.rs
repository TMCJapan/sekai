//! World layout discovery: find every `.mca` and name its namespace.
//!
//! Rationale: vanilla has two directory generations (legacy
//! `<""|DIM-1|DIM1>/<kind>/` and `dimensions/minecraft/<name>/<kind>/`),
//! Bukkit nests the legacy names one level deeper (`world/`,
//! `world_nether/DIM-1/`, `world_the_end/DIM1/`), and mods add arbitrary
//! `dimensions/<ns>/<name>/` trees. Discovery scans all of them at once -
//! new layout first on collisions - so one code path serves every server
//! generation. Custom dimensions hash their relative path to a stable
//! `Dimension` code (deterministic across runs, which history continuity
//! requires); the three vanilla codes are excluded from that range.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use sekai_core::{Dimension, RegionKind};

use crate::error::EngineError;

/// One region file found on disk with its global namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionRef {
    /// Full file path.
    pub path: PathBuf,
    /// Dimension namespace.
    pub dim: Dimension,
    /// Region family.
    pub kind: RegionKind,
    /// Region X from the file name.
    pub region_x: i32,
    /// Region Z from the file name.
    pub region_z: i32,
}

/// Which directory generation to prefer when deriving new paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutFlavor {
    /// `dimensions/minecraft/<name>/<kind>/` exists: write there.
    New,
    /// Legacy `<""|DIM-1|DIM1>/<kind>/` layout.
    Legacy,
}

/// Pick the derivation flavor: new layout wins when present.
#[must_use]
pub fn detect_flavor(world: &Path) -> LayoutFlavor {
    if world.join("dimensions").is_dir() {
        LayoutFlavor::New
    } else {
        LayoutFlavor::Legacy
    }
}

const KIND_DIRS: [(RegionKind, &str); 3] = [
    (RegionKind::REGION, "region"),
    (RegionKind::ENTITIES, "entities"),
    (RegionKind::POI, "poi"),
];

/// Stable `Dimension` code for a custom `dimensions/<ns>/<name>` tree.
///
/// Blake3 of the relative path, with the three reserved vanilla codes
/// remapped away. Cross-dimension collisions sit at 2^-32 (accepted and
/// documented); same path always yields the same code, so history stays
/// continuous across runs and machines.
fn custom_dim_id(relative: &str) -> Dimension {
    let digest = blake3::hash(relative.as_bytes());
    let bytes = digest.as_bytes();
    let mut raw = [0u8; 4];
    raw.copy_from_slice(&bytes[..4]);
    let mut id = i32::from_le_bytes(raw);
    if id == Dimension::OVERWORLD.raw()
        || id == Dimension::NETHER.raw()
        || id == Dimension::END.raw()
    {
        id = id.wrapping_add(0x0100_0000);
    }
    Dimension(id)
}

/// Read a directory, skipping it when absent; other errors propagate.
fn read_dir_opt(dir: &Path) -> Result<Vec<fs::DirEntry>, EngineError> {
    match fs::read_dir(dir) {
        Ok(iter) => {
            let mut out = Vec::new();
            for entry in iter {
                out.push(entry.map_err(|source| EngineError::Io {
                    path: dir.to_path_buf(),
                    source,
                })?);
            }
            Ok(out)
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(source) => Err(EngineError::Io {
            path: dir.to_path_buf(),
            source,
        }),
    }
}

/// Scan `<root>/<kind>/r.*.mca` into `found` (keyed for dedup).
fn scan_dim_root(
    root: &Path,
    dim: Dimension,
    found: &mut BTreeMap<(Dimension, RegionKind, i32, i32), RegionRef>,
    overwrite: bool,
) -> Result<(), EngineError> {
    for (kind, kind_dir) in KIND_DIRS {
        for entry in read_dir_opt(&root.join(kind_dir))? {
            let path = entry.path();
            if !entry
                .file_type()
                .map_err(|source| EngineError::Io {
                    path: path.clone(),
                    source,
                })?
                .is_file()
            {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_str().unwrap_or_default();
            let Ok((region_x, region_z)) = sekai_mca::parse_region_name(name) else {
                // Foreign files (temp leftovers, etc.) are not our concern.
                continue;
            };
            let reference = RegionRef {
                path: path.clone(),
                dim,
                kind,
                region_x,
                region_z,
            };
            let key = (dim, kind, region_x, region_z);
            if overwrite {
                found.insert(key, reference);
            } else {
                found.entry(key).or_insert(reference);
            }
        }
    }
    Ok(())
}

/// Legacy candidate roots: vanilla triple plus Bukkit-outer nesting.
fn legacy_roots(world: &Path) -> Result<Vec<(PathBuf, Dimension)>, EngineError> {
    let mut roots = vec![
        (world.to_path_buf(), Dimension::OVERWORLD),
        (world.join("DIM-1"), Dimension::NETHER),
        (world.join("DIM1"), Dimension::END),
    ];
    // Bukkit server root: `world/`, `world_nether/DIM-1/`, `world_the_end/DIM1/`.
    for entry in read_dir_opt(world)? {
        if !entry
            .file_type()
            .map_err(|source| EngineError::Io {
                path: entry.path(),
                source,
            })?
            .is_dir()
        {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_str().unwrap_or_default();
        if name == "world" {
            roots.push((entry.path(), Dimension::OVERWORLD));
        } else if name.ends_with("_nether") {
            roots.push((entry.path().join("DIM-1"), Dimension::NETHER));
        } else if name.ends_with("_the_end") {
            roots.push((entry.path().join("DIM1"), Dimension::END));
        }
    }
    Ok(roots)
}

/// New-layout scan: `dimensions/<ns>/<name>/<kind>/r.*.mca`.
fn scan_dimensions(
    world: &Path,
    found: &mut BTreeMap<(Dimension, RegionKind, i32, i32), RegionRef>,
) -> Result<(), EngineError> {
    let dims = world.join("dimensions");
    for ns in read_dir_opt(&dims)? {
        if !ns
            .file_type()
            .map_err(|source| EngineError::Io {
                path: ns.path(),
                source,
            })?
            .is_dir()
        {
            continue;
        }
        let ns_name = ns.file_name();
        let ns_name = ns_name.to_str().unwrap_or_default().to_string();
        for name in read_dir_opt(&ns.path())? {
            if !name
                .file_type()
                .map_err(|source| EngineError::Io {
                    path: name.path(),
                    source,
                })?
                .is_dir()
            {
                continue;
            }
            let dim_name = name.file_name();
            let dim_name = dim_name.to_str().unwrap_or_default().to_string();
            let dim = match (ns_name.as_str(), dim_name.as_str()) {
                ("minecraft", "overworld") => Dimension::OVERWORLD,
                ("minecraft", "the_nether") => Dimension::NETHER,
                ("minecraft", "the_end") => Dimension::END,
                _ => custom_dim_id(&format!("dimensions/{ns_name}/{dim_name}")),
            };
            // New layout overwrites legacy hits for one converted world.
            scan_dim_root(&name.path(), dim, found, true)?;
        }
    }
    Ok(())
}

/// Find every `.mca` under `world` with its namespace.
///
/// Missing world root is an error; missing candidate subdirectories are
/// simply skipped.
pub fn discover(world: &Path) -> Result<Vec<RegionRef>, EngineError> {
    if !world.is_dir() {
        return Err(EngineError::Io {
            path: world.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "world directory not found"),
        });
    }
    let mut found = BTreeMap::new();
    for (root, dim) in legacy_roots(world)? {
        scan_dim_root(&root, dim, &mut found, false)?;
    }
    scan_dimensions(world, &mut found)?;
    Ok(found.into_values().collect())
}

/// Derive the canonical path for a region under `flavor`.
///
/// Only vanilla namespaces are derivable; hashed custom dimensions are
/// one-way, so their missing files surface [`EngineError::UnknownRegionPath`].
pub fn derive_path(
    world: &Path,
    flavor: LayoutFlavor,
    dim: Dimension,
    kind: RegionKind,
    region_x: i32,
    region_z: i32,
) -> Result<PathBuf, EngineError> {
    let unknown = || EngineError::UnknownRegionPath {
        dim,
        kind,
        region_x,
        region_z,
    };
    let kind_dir = if kind == RegionKind::REGION {
        "region"
    } else if kind == RegionKind::ENTITIES {
        "entities"
    } else if kind == RegionKind::POI {
        "poi"
    } else {
        return Err(unknown());
    };
    let mut path = world.to_path_buf();
    match flavor {
        LayoutFlavor::New => {
            let name = if dim == Dimension::OVERWORLD {
                "overworld"
            } else if dim == Dimension::NETHER {
                "the_nether"
            } else if dim == Dimension::END {
                "the_end"
            } else {
                return Err(unknown());
            };
            path.push("dimensions");
            path.push("minecraft");
            path.push(name);
        }
        LayoutFlavor::Legacy => {
            if dim == Dimension::NETHER {
                path.push("DIM-1");
            } else if dim == Dimension::END {
                path.push("DIM1");
            } else if dim != Dimension::OVERWORLD {
                return Err(unknown());
            }
        }
    }
    path.push(kind_dir);
    path.push(format!("r.{region_x}.{region_z}.mca"));
    Ok(path)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn custom_ids_are_stable_and_reserved_free() {
        let a = custom_dim_id("dimensions/aether/sky");
        let b = custom_dim_id("dimensions/aether/sky");
        assert_eq!(a, b);
        assert_ne!(a, custom_dim_id("dimensions/aether/other"));
        assert_ne!(a, Dimension::OVERWORLD);
        assert_ne!(a, Dimension::NETHER);
        assert_ne!(a, Dimension::END);
    }

    #[test]
    fn derives_both_layouts() {
        let world = Path::new("/w");
        assert_eq!(
            derive_path(
                world,
                LayoutFlavor::Legacy,
                Dimension::NETHER,
                RegionKind::REGION,
                -1,
                2
            )
            .expect("vanilla legacy must derive"),
            Path::new("/w/DIM-1/region/r.-1.2.mca")
        );
        assert_eq!(
            derive_path(
                world,
                LayoutFlavor::New,
                Dimension::END,
                RegionKind::POI,
                0,
                0
            )
            .expect("vanilla new must derive"),
            Path::new("/w/dimensions/minecraft/the_end/poi/r.0.0.mca")
        );
        assert!(matches!(
            derive_path(
                world,
                LayoutFlavor::New,
                Dimension(4242),
                RegionKind::REGION,
                0,
                0
            ),
            Err(EngineError::UnknownRegionPath { .. })
        ));
        assert!(matches!(
            derive_path(
                world,
                LayoutFlavor::Legacy,
                Dimension::OVERWORLD,
                RegionKind(9),
                0,
                0
            ),
            Err(EngineError::UnknownRegionPath { .. })
        ));
    }
}
