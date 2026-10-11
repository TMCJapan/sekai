//! World layout discovery: find every `.mca` and name its namespace.
//!
//! Rationale: three server families share the `.mca` format but not the
//! directory layout. Vanilla uses one folder (`region/`, `DIM-1/`,
//! `DIM1/`, and since 26.1 `dimensions/minecraft/<name>/`). Bukkit-family
//! servers (Bukkit/Spigot/Paper/Purpur, pre-26.1 layout) split dimensions
//! into `<base>/`, `<base>_nether/DIM-1/`, `<base>_the_end/DIM1/`, where
//! `base` is `level-name` (`world` by default); Paper 26.1+ migrates to the
//! vanilla layout. Plugin worlds (Multiverse et al.) are arbitrary folders.
//!
//! Namespaces follow one rule: the exact default trio and vanilla trees
//! keep vanilla codes (derivable, history-stable); every other folder gets
//! a stable hash of its root-relative path, so distinct worlds can never
//! silently share coordinates. Non-derivable codes surface
//! `UnknownRegionPath` for missing files instead of writing somewhere
//! wrong; rollback restores those through their discovered folders.
//!
//! Trio detection only runs on container roots (arguments that are not
//! world folders themselves), so a lone nested folder can never steal the
//! vanilla namespace of the root's own content.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use sekai_core::{Dimension, RegionKey, RegionKind};

use crate::error::WorldError;

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

/// The single layout elected for a root.
///
/// Exactly one layout is ever respected: files belonging to any other
/// layout (migration leftovers, a stray `dimensions/` tree) are ignored
/// instead of being merged or overwritten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Layout {
    /// Legacy vanilla world: `<root>/`, `<root>/DIM-1`, `<root>/DIM1`.
    LegacyVanilla,
    /// Vanilla 26.1 world: `<root>/dimensions/minecraft/<name>`.
    Vanilla,
    /// Legacy Bukkit container: `<base>/`, `<base>_nether/DIM-1`,
    /// `<base>_the_end/DIM1`.
    LegacyPaper { base: String },
    /// Paper 26.1+ container: `<base>/dimensions/minecraft/<name>`.
    Paper { base: String },
}

impl Layout {
    /// The three derivable vanilla dimension directories of this layout.
    pub(crate) fn vanilla_dirs(&self, root: &Path) -> [(Dimension, PathBuf); 3] {
        match self {
            Self::LegacyVanilla => [
                (Dimension::OVERWORLD, root.to_path_buf()),
                (Dimension::NETHER, root.join("DIM-1")),
                (Dimension::END, root.join("DIM1")),
            ],
            Self::Vanilla => minecraft_trio(&root.join("dimensions").join("minecraft")),
            Self::LegacyPaper { base } => [
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
            Self::Paper { base } => {
                minecraft_trio(&root.join(base).join("dimensions").join("minecraft"))
            }
        }
    }

    /// The elected `dimensions/<ns>/<name>` tree, when the layout has one.
    pub(crate) fn dimensions_dir(&self, root: &Path) -> Option<PathBuf> {
        match self {
            Self::Vanilla => Some(root.join("dimensions")),
            Self::Paper { base } => Some(root.join(base).join("dimensions")),
            _ => None,
        }
    }

    /// Base folders of the elected layout; they are part of the election
    /// and must not be rediscovered as plugin worlds.
    pub(crate) fn trio_names(&self) -> Vec<String> {
        match self {
            Self::LegacyPaper { base } | Self::Paper { base } => vec![
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
        root: &Path,
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
        let dir = self
            .vanilla_dirs(root)
            .into_iter()
            .find_map(|(d, dir)| (d == dim).then_some(dir))
            .ok_or_else(unknown)?;
        let kind_dir = kind_dir(kind).ok_or_else(unknown)?;
        Ok(dir
            .join(kind_dir)
            .join(format!("r.{region_x}.{region_z}.mca")))
    }
}

/// `dimensions/minecraft/<name>` directories for the three vanilla dims.
fn minecraft_trio(minecraft: &Path) -> [(Dimension, PathBuf); 3] {
    [
        (Dimension::OVERWORLD, minecraft.join("overworld")),
        (Dimension::NETHER, minecraft.join("the_nether")),
        (Dimension::END, minecraft.join("the_end")),
    ]
}

/// Elect the single layout for `root`.
///
/// Trio election comes first: a Bukkit container root may hold a stray
/// `dimensions/` directory (migration leftovers, a plugin), and deriving
/// vanilla namespaces from it would restore into a tree the server never
/// reads. A complete trio whose base holds a `dimensions/` directory is
/// Paper 26.1+; otherwise it is the legacy Bukkit layout. World roots
/// (arguments that are world folders themselves) elect vanilla 26.1 when
/// they hold a `dimensions/` tree, legacy otherwise.
///
/// Pass the same root on every run: the election depends on the tree on
/// disk.
pub(crate) fn resolve(root: impl AsRef<Path>) -> Result<Layout, WorldError> {
    let root = root.as_ref();
    if !is_top_world_folder(root)?
        && let Some(base) = bukkit_base(root)?
    {
        if root.join(&base).join("dimensions").is_dir() {
            return Ok(Layout::Paper { base });
        }
        return Ok(Layout::LegacyPaper { base });
    }
    if root.join("dimensions").is_dir() {
        return Ok(Layout::Vanilla);
    }
    Ok(Layout::LegacyVanilla)
}

const KIND_DIRS: [(RegionKind, &str); 3] = [
    (RegionKind::REGION, "region"),
    (RegionKind::ENTITIES, "entities"),
    (RegionKind::POI, "poi"),
];

/// Subdirectories never treated as world folders themselves.
const RESERVED_SUBDIRS: [&str; 3] = ["dimensions", "DIM-1", "DIM1"];

fn read_dir_opt(dir: impl AsRef<Path>) -> Result<Vec<fs::DirEntry>, WorldError> {
    match fs::read_dir(dir.as_ref()) {
        Ok(entries) => entries
            .map(|res| res.map_err(|source| WorldError::io(dir.as_ref(), source)))
            .collect(),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(source) => Err(WorldError::io(dir.as_ref(), source)),
    }
}

fn is_dir(entry: &fs::DirEntry) -> Result<bool, WorldError> {
    resolved_meta(entry).map_or(Ok(false), |meta| Ok(meta.is_dir()))
}

fn is_file(entry: &fs::DirEntry) -> Result<bool, WorldError> {
    resolved_meta(entry).map_or(Ok(false), |meta| Ok(meta.is_file()))
}

/// Metadata of the entry's target, following symlinks (`fs::metadata`, unlike
/// `DirEntry::file_type` and `DirEntry::metadata`): a region file or world
/// folder living on another disk is still part of the world, and the game
/// itself opens whatever the path resolves to. Ignoring links made such a
/// world invisible - the backup stored nothing and the next one tombstoned
/// the history the previous backup had recorded.
///
/// Unresolvable targets - dangling links, permission denials - yield `None`
/// and are skipped: an unreadable entry must not fail the whole scan.
fn resolved_meta(entry: &fs::DirEntry) -> Option<fs::Metadata> {
    fs::metadata(entry.path()).ok()
}

/// Every region file found so far, keyed by its global namespace.
type Found = BTreeMap<(Dimension, RegionKind, i32, i32), RegionRef>;

fn scan_dim_root(
    root: impl AsRef<Path>,
    dim: Dimension,
    found: &mut Found,
) -> Result<(), WorldError> {
    for (kind, kind_dir) in KIND_DIRS {
        // Sorted by name so that two files spelling the same coordinates
        // (`r.0.0.mca` and `r.00.00.mca`, both of which the game itself
        // parses as region 0,0) resolve the same way on every run instead
        // of following directory order.
        let mut entries = read_dir_opt(root.as_ref().join(kind_dir))?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            if !is_file(&entry)? {
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
            found
                .entry((dim, kind, region_x, region_z))
                .or_insert(reference);
        }
    }
    Ok(())
}

/// Whether `dir` looks like a world folder: `level.dat`, `DIM-1`/`DIM1`
/// nesting, or a kind directory holding parseable region files.
fn is_world_folder(dir: impl AsRef<Path>) -> Result<bool, WorldError> {
    Ok(is_top_world_folder(dir.as_ref())?
        || dir.as_ref().join("DIM-1").is_dir()
        || dir.as_ref().join("DIM1").is_dir())
}

/// Whether `dir` is itself a world folder root: `level.dat` or a kind
/// directory holding parseable region files (no `DIM-1` nesting test - a
/// bare `DIM-1` holder is a container, and claiming it as a world would
/// hide the trio logic from `discover`).
fn is_top_world_folder(dir: impl AsRef<Path>) -> Result<bool, WorldError> {
    if dir.as_ref().join("level.dat").is_file() {
        return Ok(true);
    }
    for (_, kind_dir) in KIND_DIRS {
        for entry in read_dir_opt(dir.as_ref().join(kind_dir))? {
            if !is_file(&entry)? {
                continue;
            }
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if sekai_anvil::parse_region_name(name).is_ok() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Bukkit overworld folder name (`level-name`, `world` by default).
///
/// Only complete trios (`<base>`, `<base>_nether`, `<base>_the_end`)
/// elect the Bukkit layout; a lone folder (e.g. a Multiverse world `sky/`)
/// without its `_nether`/`_the_end` siblings is treated as a plugin world
/// and hashed, avoiding a silent namespace flip if siblings appear later.
///
/// The overworld candidate is recognized structurally (its own `region/`
/// directory, which a server creates whether or not it holds files yet):
/// requiring stored region files would flip the whole world to hashed
/// namespaces the moment the overworld is emptied or briefly unsaved.
fn bukkit_base(world: impl AsRef<Path>) -> Result<Option<String>, WorldError> {
    let mut trio: Vec<String> = Vec::new();
    for entry in read_dir_opt(world.as_ref())? {
        if !is_dir(&entry)? {
            continue;
        }
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if RESERVED_SUBDIRS.contains(&name) {
            continue;
        }
        if !is_bukkit_overworld(entry.path())? {
            continue;
        }
        if world.as_ref().join(format!("{name}_nether")).is_dir()
            || world.as_ref().join(format!("{name}_the_end")).is_dir()
        {
            trio.push(name.to_owned());
        }
    }
    // `level-name` is authoritative: a leftover `world/` trio must never
    // win over the world the server actually loads.
    if let Some(level) = level_name(world.as_ref())
        && trio.contains(&level)
    {
        return Ok(Some(level));
    }
    if trio.iter().any(|t| t == "world") {
        return Ok(Some("world".to_owned()));
    }
    trio.sort();
    Ok(trio.into_iter().next())
}

/// Overworld folder signature: world-folder contents, or a `region/`
/// directory that exists but is still empty.
fn is_bukkit_overworld(dir: impl AsRef<Path>) -> Result<bool, WorldError> {
    Ok(dir.as_ref().join("region").is_dir() || is_world_folder(dir)?)
}

/// `level-name` from a Bukkit/Paper `server.properties`, when readable.
///
/// The file is untrusted input: only uncommented `key=value` lines with
/// that exact key count, and the value is trimmed of surrounding spaces.
/// An unreadable file is simply "not configured" - it never fails a scan.
fn level_name(world: impl AsRef<Path>) -> Option<String> {
    let text = fs::read_to_string(world.as_ref().join("server.properties")).ok()?;
    text.lines().find_map(|line| {
        line.trim()
            .strip_prefix("level-name=")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

/// Vanilla namespace for a `(namespace, name)` dimensions tree, if any.
fn tree_dim(ns_name: &str, dim_name: &str) -> Option<Dimension> {
    match (ns_name, dim_name) {
        ("minecraft", "overworld") => Some(Dimension::OVERWORLD),
        ("minecraft", "the_nether") => Some(Dimension::NETHER),
        ("minecraft", "the_end") => Some(Dimension::END),
        _ => None,
    }
}

/// Dimension code for the elected world's `dimensions/<ns>/<name>` tree.
///
/// `minecraft/overworld`, `minecraft/the_nether`, and `minecraft/the_end`
/// keep vanilla codes so history survives 26.1-style migrations; every
/// other pair hashes its `<namespace>:<name>` identifier.
fn elected_dimension(ns_name: &str, dim_name: &str) -> Dimension {
    tree_dim(ns_name, dim_name)
        .unwrap_or_else(|| sekai_core::resolve_custom_dimension(&format!("{ns_name}:{dim_name}")))
}

/// Dimension code inside a plugin world named `base`.
///
/// The world's own `minecraft/*` tree hashes `<base>:<name>` so it can
/// never alias the elected world's vanilla codes; datapack dimensions keep
/// their `<namespace>:<name>` identity.
fn plugin_dimension(base: &str, ns_name: &str, dim_name: &str) -> Dimension {
    if ns_name == "minecraft" {
        sekai_core::resolve_custom_dimension(&format!("{base}:{dim_name}"))
    } else {
        sekai_core::resolve_custom_dimension(&format!("{ns_name}:{dim_name}"))
    }
}

/// Scan a `dimensions/<ns>/<name>` tree, naming every dimension through
/// `dimension`.
fn scan_dimension_trees(
    dims_dir: impl AsRef<Path>,
    dimension: impl Fn(&str, &str) -> Dimension,
    found: &mut Found,
) -> Result<(), WorldError> {
    for ns in read_dir_opt(dims_dir)? {
        if !is_dir(&ns)? {
            continue;
        }
        let ns_file_name = ns.file_name();
        let Some(ns_name) = ns_file_name.to_str() else {
            continue;
        };
        for name in read_dir_opt(ns.path())? {
            if !is_dir(&name)? {
                continue;
            }
            let dim_file_name = name.file_name();
            let Some(dim_name) = dim_file_name.to_str() else {
                continue;
            };
            scan_dim_root(name.path(), dimension(ns_name, dim_name), found)?;
        }
    }
    Ok(())
}

/// Scan plugin worlds (Multiverse et al.) beside the elected world.
///
/// Each plugin world elects exactly one of the four layouts itself and
/// keeps every dimension under hashed namespaces: the base folder names the
/// world, the logical dimension names the dimension. Bukkit-style
/// `<base>_nether`/`<base>_the_end` siblings are consumed by their base so
/// they are never discovered twice.
fn scan_plugin_worlds(root: &Path, skip: &[String], found: &mut Found) -> Result<(), WorldError> {
    let mut skipped: BTreeSet<String> = RESERVED_SUBDIRS.iter().map(|s| (*s).to_owned()).collect();
    skipped.extend(skip.iter().cloned());

    // Sorted so a `<base>` is always seen before its `_nether`/`_the_end`
    // siblings and consumes them deterministically.
    let mut entries = read_dir_opt(root)?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        if !is_dir(&entry)? {
            continue;
        }
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if skipped.contains(name) {
            continue;
        }
        let dir = entry.path();
        let siblings = root.join(format!("{name}_nether")).is_dir()
            || root.join(format!("{name}_the_end")).is_dir();
        let is_world = if siblings {
            is_bukkit_overworld(&dir)?
        } else {
            is_world_folder(&dir)?
        };
        if !is_world {
            continue;
        }

        if dir.join("dimensions").is_dir() {
            scan_dimension_trees(
                dir.join("dimensions"),
                |ns, dim| plugin_dimension(name, ns, dim),
                found,
            )?;
        } else {
            let over = plugin_dimension(name, "minecraft", "overworld");
            scan_dim_root(&dir, over, found)?;
            let nether = if siblings {
                root.join(format!("{name}_nether")).join("DIM-1")
            } else {
                dir.join("DIM-1")
            };
            scan_dim_root(
                &nether,
                plugin_dimension(name, "minecraft", "the_nether"),
                found,
            )?;
            let end = if siblings {
                root.join(format!("{name}_the_end")).join("DIM1")
            } else {
                dir.join("DIM1")
            };
            scan_dim_root(&end, plugin_dimension(name, "minecraft", "the_end"), found)?;
        }
        skipped.insert(format!("{name}_nether"));
        skipped.insert(format!("{name}_the_end"));
    }
    Ok(())
}

/// Find every `.mca` under `root` with its namespace.
///
/// Only the elected [`Layout`] is scanned; files belonging to any other
/// layout (conversion leftovers, a stray `dimensions/` tree) stay
/// invisible. Other world folders (Multiverse et al.) are still found,
/// each under its own hashed namespaces. Missing world root is an error;
/// missing candidate subdirectories are simply skipped.
pub(crate) fn discover(
    root: impl AsRef<Path>,
    layout: &Layout,
) -> Result<Vec<RegionRef>, WorldError> {
    let root = root.as_ref();
    if !root.is_dir() {
        return Err(WorldError::io(
            root,
            std::io::Error::new(std::io::ErrorKind::NotFound, "world directory not found"),
        ));
    }
    let mut found = Found::new();
    for (dim, dir) in layout.vanilla_dirs(root) {
        scan_dim_root(&dir, dim, &mut found)?;
    }
    if let Some(dims_dir) = layout.dimensions_dir(root) {
        scan_dimension_trees(dims_dir, elected_dimension, &mut found)?;
    }
    scan_plugin_worlds(root, &layout.trio_names(), &mut found)?;
    Ok(found.into_values().collect())
}

const fn kind_dir(kind: RegionKind) -> Option<&'static str> {
    match kind {
        RegionKind::REGION => Some("region"),
        RegionKind::ENTITIES => Some("entities"),
        RegionKind::POI => Some("poi"),
        _ => None,
    }
}

/// Path for a missing region inside a live sibling's directory, if any.
///
/// Folders may have moved since the backup (e.g. across a 26.1 migration),
/// so callers prefer this over derived paths whenever a same-dimension
/// sibling exists.
pub fn sibling_path(discovered: &BTreeMap<RegionKey, PathBuf>, key: &RegionKey) -> Option<PathBuf> {
    let sibling = discovered
        .iter()
        .find(|(k, _)| k.dim == key.dim && k.kind == key.kind)
        .map(|(_, path)| path)?;
    let mut path = sibling.clone();
    path.set_file_name(format!("r.{}.{}.mca", key.rx, key.rz));
    Some(path)
}
