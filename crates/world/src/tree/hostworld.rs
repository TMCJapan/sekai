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
mod lock;
mod vanilla;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sekai_core::{Dimension, RegionFingerprint, RegionKey, RegionKind};

use super::WorldTree;
use crate::region::RegionFile;
use crate::{WorldError, WorldLockMode};

pub use lock::HostWorldLock;

/// Directory of `kind` under a dimension directory.
const fn kind_dir(kind: RegionKind) -> &'static str {
    match kind {
        RegionKind::Region => "region",
        RegionKind::Entities => "entities",
        RegionKind::Poi => "poi",
    }
}

/// One region file of a host world: the path it lives at.
///
/// The operations never see a path - they work on the handle. A
/// host-specific caller (the CLI, a test) can still ask for the path
/// with [`HostRegion::as_path`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostRegion(PathBuf);

impl HostRegion {
    /// The file's path on disk.
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl std::fmt::Display for HostRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.display().fmt(f)
    }
}

impl RegionFile for HostRegion {
    type Error = WorldError;

    fn read(&self) -> Result<Vec<u8>, WorldError> {
        crate::swap::open_image(&self.0)
    }

    fn fingerprint(&self, key: RegionKey) -> Result<RegionFingerprint, WorldError> {
        crate::fingerprint::fingerprint_file(&self.0, key)
    }

    fn exists(&self) -> bool {
        self.0.exists()
    }

    fn write(&self, image: &[u8]) -> Result<(), WorldError> {
        crate::swap::atomic_swap(&self.0, image)
    }

    fn remove(&self) -> Result<(), WorldError> {
        std::fs::remove_file(&self.0).map_err(|source| WorldError::io(&self.0, source))
    }
}

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
    fn check_root(&self) -> Result<(), WorldError> {
        if self.root.is_dir() {
            return Ok(());
        }
        Err(WorldError::io(
            &self.root,
            std::io::Error::new(std::io::ErrorKind::NotFound, "world directory not found"),
        ))
    }
}

/// Collect dimension directories, attaching the failing path to I/O errors.
fn collect_dimensions(dir: impl AsRef<Path>) -> Result<BTreeMap<Dimension, PathBuf>, WorldError> {
    let dir = dir.as_ref();
    vanilla::collect_dimensions(dir).map_err(|source| WorldError::io(dir, source))
}

impl WorldTree for HostWorldTree {
    type Error = WorldError;
    type Region = HostRegion;
    type Lock = HostWorldLock;

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
                Err(source) => return Err(WorldError::io(path, source)),
            }
        }
        Ok(true)
    }

    fn get_dims(&self) -> Result<Vec<Dimension>, Self::Error> {
        self.check_root()?;
        Ok(self.dirs.keys().copied().collect())
    }

    fn get_regions(
        &self,
        dim: Dimension,
        kind: RegionKind,
    ) -> Result<Vec<(RegionKey, Self::Region)>, Self::Error> {
        let Some(dir) = self.dirs.get(&dim) else {
            return Ok(Vec::new());
        };

        let name = kind_dir(kind);
        let mut regions: BTreeMap<(i32, i32), PathBuf> = BTreeMap::new();
        // Sorted by name so the reported pair is the same on every run.
        let kind_dir_path = dir.join(name);
        let mut entries = vanilla::read_dir_opt(&kind_dir_path)
            .map_err(|source| WorldError::io(&kind_dir_path, source))?;
        entries.sort_unstable_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            if !vanilla::is_file(&entry) {
                continue;
            }
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            let Ok((rx, rz)) = sekai_anvil::parse_region_name(name) else {
                continue;
            };
            let path = entry.path();
            // Two spellings of one coordinate (`r.0.0.mca` and `r.00.00.mca`,
            // both of which the game itself parses as region 0,0) are
            // ambiguous: the game reads whichever file it finds, so the tool
            // must not pick one and drop the other.
            if let Some(previous) = regions.get(&(rx, rz)) {
                return Err(WorldError::AmbiguousRegion {
                    dim,
                    kind,
                    region_x: rx,
                    region_z: rz,
                    first: previous.clone(),
                    second: path,
                });
            }
            // The BTreeMap keeps the result ordered by coordinates
            // regardless of `RegionKey` field order.
            regions.insert((rx, rz), path);
        }
        Ok(regions
            .into_iter()
            .map(|((rx, rz), path)| (RegionKey { dim, kind, rx, rz }, HostRegion(path)))
            .collect())
    }

    fn derive_region(
        &self,
        RegionKey { dim, kind, rx, rz }: RegionKey,
    ) -> Result<Self::Region, Self::Error> {
        // Mirrors `WorldError::UnknownRegionPath`; the CLI troubleshooting
        // entry refers to this wording.
        let cannot_derive = || WorldError::UnknownRegionPath {
            dim,
            kind,
            region_x: rx,
            region_z: rz,
        };
        let root = self.dirs.get(&dim).ok_or_else(cannot_derive)?;
        let name = kind_dir(kind);

        Ok(HostRegion(root.join(name).join(format!("r.{rx}.{rz}.mca"))))
    }

    fn sibling_region(
        &self,
        region: &Self::Region,
        key: RegionKey,
    ) -> Result<Self::Region, Self::Error> {
        // The directory is inherited from the region, so a key from another
        // dimension or family would land in the wrong folder. The caller
        // must not do that; the check catches it in tests and debug builds.
        debug_assert!(
            self.derive_region(key)
                .is_ok_and(|derived| derived.0.parent() == region.0.parent()),
            "sibling key must share the observed region's dimension and family"
        );
        let RegionKey { rx, rz, .. } = key;
        // The sibling sits in the directory the region was observed in, so
        // a world whose folders moved keeps restoring into the live one.
        Ok(HostRegion(
            region.0.with_file_name(format!("r.{rx}.{rz}.mca")),
        ))
    }

    fn lock(&self, mode: WorldLockMode) -> Result<Option<Self::Lock>, Self::Error> {
        // The lock lives in the root as passed, so two roots naming one
        // world (a Bukkit server root and its world folder) do not exclude
        // each other (ADR-0014). A mistyped world path should read as one,
        // not as a lock failure.
        self.check_root()?;
        let path = self.root.join(HostWorldLock::FILE_NAME);
        match HostWorldLock::acquire(&path, mode) {
            Ok(lock) => Ok(lock),
            // Contention is its own failure, not an I/O one: a caller has to
            // tell "busy, retry" from a lock file it cannot open at all.
            Err(source) if source.kind() == std::io::ErrorKind::WouldBlock => {
                Err(WorldError::WorldLocked { path })
            }
            Err(source) => Err(WorldError::io(path, source)),
        }
    }
}
