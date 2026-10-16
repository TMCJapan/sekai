use std::path::{Path, PathBuf};

use sekai_core::{Dimension, RegionKind};

use super::WorldTree;
use crate::discover::{self, minecraft_dim, minecraft_trio};
use crate::{RegionRef, WorldError};

/// A [`WorldTree`] rooted at a local directory, tagged with the single
/// layout elected when it was opened.
///
/// [`HostWorldTree::new`] inspects the disk once and picks exactly one
/// variant; every later operation scans and derives through that variant
/// alone, so files belonging to any other layout (conversion leftovers, a
/// stray `dimensions/` tree) stay invisible instead of being merged.
///
/// The root is stored as given, never canonicalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostWorldTree {
    /// 26.1+ vanilla world: `<root>/dimensions/minecraft/<name>`.
    Vanilla { root: PathBuf },
    /// Pre-26.1 vanilla world: `<root>/`, `<root>/DIM-1`, `<root>/DIM1`.
    VanillaLegacy { root: PathBuf },
    /// 26.1+ Bukkit-family container (Paper):
    /// `<root>/<base>/dimensions/minecraft/<name>`.
    Bukkit { root: PathBuf, base: String },
    /// Pre-26.1 Bukkit-family container: `<root>/<base>`,
    /// `<root>/<base>_nether/DIM-1`, `<root>/<base>_the_end/DIM1`.
    BukkitLegacy { root: PathBuf, base: String },
}

impl HostWorldTree {
    /// Elect the layout of the tree rooted at `root`.
    ///
    /// Detection runs narrow to broad: a complete Bukkit trio elects a
    /// Bukkit layout (26.1 when its base holds a `dimensions/minecraft`
    /// tree, legacy otherwise), then a root-level `dimensions/minecraft`
    /// tree elects [`Vanilla`](Self::Vanilla), and everything else resolves
    /// as [`VanillaLegacy`](Self::VanillaLegacy). A missing root resolves
    /// as an empty legacy tree and later discovery fails loudly; an
    /// unreadable root fails here.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, WorldError> {
        let root = root.as_ref();
        if !discover::is_top_world_folder(root)?
            && let Some(base) = discover::bukkit_base(root)?
        {
            let root = root.to_path_buf();
            return Ok(if discover::has_minecraft_dimensions(root.join(&base)) {
                Self::Bukkit { root, base }
            } else {
                Self::BukkitLegacy { root, base }
            });
        }
        let root = root.to_path_buf();
        Ok(if discover::has_minecraft_dimensions(&root) {
            Self::Vanilla { root }
        } else {
            Self::VanillaLegacy { root }
        })
    }

    /// Fresh output tree in the legacy single-folder layout (`region/`,
    /// `DIM-1/`, `DIM1/`), for roots that do not exist yet (export).
    pub fn new_legacy(root: impl AsRef<Path>) -> Self {
        Self::VanillaLegacy {
            root: root.as_ref().to_path_buf(),
        }
    }

    /// Fresh output tree in the 26.1 layout
    /// (`dimensions/minecraft/<name>/`), for roots that do not exist yet
    /// (export).
    pub fn new_dimensions(root: impl AsRef<Path>) -> Self {
        Self::Vanilla {
            root: root.as_ref().to_path_buf(),
        }
    }

    /// Fresh output tree in the 26.1 Bukkit layout around `base`: the
    /// split `<base>/` folder holds `dimensions/minecraft/<name>/`, for
    /// roots that do not exist yet (export).
    pub fn new_bukkit(root: impl AsRef<Path>, base: impl Into<String>) -> Self {
        Self::Bukkit {
            root: root.as_ref().to_path_buf(),
            base: base.into(),
        }
    }

    /// Fresh output tree in the legacy Bukkit split-folder layout around
    /// `base` (`<base>/`, `<base>_nether/DIM-1/`,
    /// `<base>_the_end/DIM1/`), for roots that do not exist yet (export).
    pub fn new_bukkit_legacy(root: impl AsRef<Path>, base: impl Into<String>) -> Self {
        Self::BukkitLegacy {
            root: root.as_ref().to_path_buf(),
            base: base.into(),
        }
    }

    /// Root path as given to the constructor.
    pub fn root(&self) -> &Path {
        match self {
            Self::Vanilla { root }
            | Self::VanillaLegacy { root }
            | Self::Bukkit { root, .. }
            | Self::BukkitLegacy { root, .. } => root,
        }
    }

    /// Folder holding the elected world: the root itself for vanilla
    /// layouts, `<root>/<base>` for Bukkit ones.
    fn world_dir(&self) -> PathBuf {
        match self {
            Self::Vanilla { root } | Self::VanillaLegacy { root } => root.clone(),
            Self::Bukkit { root, base } | Self::BukkitLegacy { root, base } => root.join(base),
        }
    }

    /// The elected world's `dimensions/` tree, where datapack dimensions
    /// live in every layout.
    pub(crate) fn datapack_dims(&self) -> PathBuf {
        self.world_dir().join("dimensions")
    }

    /// The three derivable vanilla dimension directories of this layout.
    pub(crate) fn vanilla_dirs(&self) -> [(Dimension, PathBuf); 3] {
        match self {
            Self::Vanilla { root } => minecraft_trio(root.join("dimensions").join("minecraft")),
            Self::VanillaLegacy { root } => [
                (Dimension::OVERWORLD, root.clone()),
                (Dimension::NETHER, root.join("DIM-1")),
                (Dimension::END, root.join("DIM1")),
            ],
            Self::Bukkit { root, base } => {
                minecraft_trio(root.join(base).join("dimensions").join("minecraft"))
            }
            Self::BukkitLegacy { root, base } => [
                (Dimension::OVERWORLD, root.join(base)),
                (
                    Dimension::NETHER,
                    root.join(format!("{base}_nether")).join("DIM-1"),
                ),
                (
                    Dimension::END,
                    root.join(format!("{base}_the_end")).join("DIM1"),
                ),
            ],
        }
    }

    /// Base folders of a Bukkit layout; they belong to the elected world
    /// and must not be rediscovered as plugin worlds.
    pub(crate) fn trio_names(&self) -> Vec<String> {
        match self {
            Self::Bukkit { base, .. } | Self::BukkitLegacy { base, .. } => vec![
                base.clone(),
                format!("{base}_nether"),
                format!("{base}_the_end"),
            ],
            _ => Vec::new(),
        }
    }

    /// Canonical path for one region file under the elected layout.
    ///
    /// Only vanilla namespaces are derivable; hashed custom dimensions
    /// surface [`WorldError::UnknownRegionPath`] instead of writing
    /// somewhere wrong.
    pub(crate) fn region_path(
        &self,
        dim: Dimension,
        kind: RegionKind,
        region_x: i32,
        region_z: i32,
    ) -> Result<PathBuf, WorldError> {
        let unknown = || WorldError::UnknownRegionPath {
            dim,
            kind,
            region_x,
            region_z,
        };
        // Derive exactly the requested directory: building the whole
        // `vanilla_dirs` array (with its `format!` siblings) on every
        // per-region derivation is needless allocation.
        let dir = match self {
            Self::Vanilla { root } => minecraft_dim(root, dim).ok_or_else(unknown)?,
            Self::VanillaLegacy { root } => match dim {
                Dimension::OVERWORLD => root.clone(),
                Dimension::NETHER => root.join("DIM-1"),
                Dimension::END => root.join("DIM1"),
                _ => return Err(unknown()),
            },
            Self::Bukkit { root, base } => {
                minecraft_dim(root.join(base), dim).ok_or_else(unknown)?
            }
            Self::BukkitLegacy { root, base } => match dim {
                Dimension::OVERWORLD => root.join(base),
                Dimension::NETHER => root.join(format!("{base}_nether")).join("DIM-1"),
                Dimension::END => root.join(format!("{base}_the_end")).join("DIM1"),
                _ => return Err(unknown()),
            },
        };
        let kind_dir = discover::kind_dir(kind).ok_or_else(unknown)?;
        Ok(dir
            .join(kind_dir)
            .join(format!("r.{region_x}.{region_z}.mca")))
    }
}

impl WorldTree for HostWorldTree {
    type Error = WorldError;

    fn is_empty(&self) -> Result<bool, Self::Error> {
        match std::fs::read_dir(self.root()) {
            Ok(mut entries) => Ok(entries.next().is_none()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(source) => Err(WorldError::io(self.root(), source)),
        }
    }

    fn discover(&self) -> Result<Vec<RegionRef>, Self::Error> {
        discover::discover(self)
    }

    fn derive_path(
        &self,
        dim: Dimension,
        kind: RegionKind,
        region_x: i32,
        region_z: i32,
    ) -> Result<PathBuf, Self::Error> {
        self.region_path(dim, kind, region_x, region_z)
    }
}
