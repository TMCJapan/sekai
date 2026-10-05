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

use std::collections::BTreeMap;
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

/// Directory generation for deriving new paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutFlavor {
    /// `dimensions/minecraft/<name>/<kind>/` exists: write there (26.1+).
    New,
    /// Legacy `<""|DIM-1|DIM1>/<kind>/` layout.
    Legacy,
    /// Bukkit-family split folders (`<base>/`, `<base>_nether/DIM-1/`,
    /// `<base>_the_end/DIM1/`), where `base` is the overworld folder name.
    Bukkit {
        /// Overworld folder name (`world` by default, else `level-name`).
        base: String,
    },
}

/// Pick the derivation flavor: an elected Bukkit trio first, then the new
/// layout when present, else legacy.
///
/// Pass the same path on every run: namespace codes for non-default
/// folders derive from root-relative paths.
pub(crate) fn detect_flavor(world: impl AsRef<Path>) -> Result<LayoutFlavor, WorldError> {
    // Trio first: a Bukkit container root may hold a stray `dimensions/`
    // directory (migration leftovers, a plugin), and deriving vanilla
    // namespaces from it would restore into a tree the server never reads.
    let container = !is_top_world_folder(world.as_ref())?;
    if container && let Some(base) = bukkit_base(world.as_ref())? {
        return Ok(LayoutFlavor::Bukkit { base });
    }
    if world.as_ref().join("dimensions").is_dir() {
        return Ok(LayoutFlavor::New);
    }
    Ok(LayoutFlavor::Legacy)
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

#[derive(Clone, Copy)]
enum InsertPolicy {
    KeepExisting,
    Overwrite,
}

fn scan_dim_root(
    root: impl AsRef<Path>,
    dim: Dimension,
    found: &mut BTreeMap<(Dimension, RegionKind, i32, i32), RegionRef>,
    policy: InsertPolicy,
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
            let key = (dim, kind, region_x, region_z);
            match policy {
                InsertPolicy::Overwrite => {
                    found.insert(key, reference);
                }
                InsertPolicy::KeepExisting => {
                    found.entry(key).or_insert(reference);
                }
            }
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
/// elect a Bukkit flavor; a lone folder (e.g. a Multiverse world `sky/`)
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

/// Root-relative `/`-joined path for stable hashing, or `None` for
/// non-UTF-8 names (skipped like everywhere else here).
fn rel_name(world: impl AsRef<Path>, path: impl AsRef<Path>) -> Option<String> {
    let rel = path.as_ref().strip_prefix(world).ok()?;
    let mut out = String::new();
    for (i, comp) in rel.components().enumerate() {
        let s = comp.as_os_str().to_str()?;
        if i > 0 {
            out.push('/');
        }
        out.push_str(s);
    }
    Some(out)
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

/// Scan `dimensions/<ns>/<name>/<kind>/r.*.mca` under `dims_dir`.
///
/// `rel_prefix` is the root-relative folder path (`""` for the root
/// itself); non-vanilla trees hash `{prefix}/dimensions/<ns>/<name>`.
/// Vanilla `(namespace, name)` pairs always map to vanilla codes so history
/// survives 26.1-style migrations.
fn scan_dimensions(
    dims_dir: impl AsRef<Path>,
    rel_prefix: &str,
    found: &mut BTreeMap<(Dimension, RegionKind, i32, i32), RegionRef>,
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
            let dim = tree_dim(ns_name, dim_name).unwrap_or_else(|| {
                let rel = if rel_prefix.is_empty() {
                    format!("dimensions/{ns_name}/{dim_name}")
                } else {
                    format!("{rel_prefix}/dimensions/{ns_name}/{dim_name}")
                };
                sekai_core::resolve_custom_dimension(&rel)
            });
            scan_dim_root(name.path(), dim, found, InsertPolicy::Overwrite)?;
        }
    }
    Ok(())
}

/// Find every `.mca` under `world` with its namespace.
///
/// Missing world root is an error; missing candidate subdirectories are
/// simply skipped. Pass a server root for Bukkit-family servers (all world
/// folders are found) or a single world folder for vanilla ones - but the
/// same path on every run, since non-default namespaces hash
/// root-relative paths.
pub(crate) fn discover(world: impl AsRef<Path>) -> Result<Vec<RegionRef>, WorldError> {
    if !world.as_ref().is_dir() {
        return Err(WorldError::io(
            world.as_ref(),
            std::io::Error::new(std::io::ErrorKind::NotFound, "world directory not found"),
        ));
    }
    let mut found = BTreeMap::new();
    // Vanilla roots at the argument itself.
    for (root, dim) in [
        (world.as_ref().to_path_buf(), Dimension::OVERWORLD),
        (world.as_ref().join("DIM-1"), Dimension::NETHER),
        (world.as_ref().join("DIM1"), Dimension::END),
    ] {
        scan_dim_root(&root, dim, &mut found, InsertPolicy::KeepExisting)?;
    }
    // Container roots (not world folders themselves) additionally resolve
    // the Bukkit trio. Gating on container mode keeps a lone nested folder
    // from stealing the vanilla namespace of the root's own content.
    let base = if is_top_world_folder(world.as_ref())? {
        None
    } else {
        bukkit_base(world.as_ref())?
    };
    // Bukkit trio: live server data wins over conversion leftovers above.
    if let Some(base) = &base {
        let over = world.as_ref().join(base);
        scan_dim_root(
            &over,
            Dimension::OVERWORLD,
            &mut found,
            InsertPolicy::Overwrite,
        )?;
        scan_dim_root(
            world.as_ref().join(format!("{base}_nether")).join("DIM-1"),
            Dimension::NETHER,
            &mut found,
            InsertPolicy::Overwrite,
        )?;
        scan_dim_root(
            world.as_ref().join(format!("{base}_the_end")).join("DIM1"),
            Dimension::END,
            &mut found,
            InsertPolicy::Overwrite,
        )?;
        scan_dimensions(over.join("dimensions"), base, &mut found)?;
    }
    // Other world folders: hashed namespaces, never colliding silently.
    let trio: [String; 3] = base.as_ref().map_or_else(Default::default, |base| {
        [
            base.clone(),
            format!("{base}_nether"),
            format!("{base}_the_end"),
        ]
    });
    for entry in read_dir_opt(world.as_ref())? {
        if !is_dir(&entry)? {
            continue;
        }
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if RESERVED_SUBDIRS.contains(&name) || trio.iter().any(|t| t == name) {
            continue;
        }
        if !is_world_folder(entry.path())? {
            continue;
        }
        let Some(rel) = rel_name(world.as_ref(), entry.path()) else {
            continue;
        };
        let dim = sekai_core::resolve_custom_dimension(&rel);
        scan_dim_root(entry.path(), dim, &mut found, InsertPolicy::KeepExisting)?;
        scan_dim_root(
            entry.path().join("DIM-1"),
            sekai_core::resolve_custom_dimension(&format!("{rel}/DIM-1")),
            &mut found,
            InsertPolicy::KeepExisting,
        )?;
        scan_dim_root(
            entry.path().join("DIM1"),
            sekai_core::resolve_custom_dimension(&format!("{rel}/DIM1")),
            &mut found,
            InsertPolicy::KeepExisting,
        )?;
        scan_dimensions(entry.path().join("dimensions"), &rel, &mut found)?;
    }
    // Root-level dimensions tree (vanilla 26.1+ single world).
    scan_dimensions(world.as_ref().join("dimensions"), "", &mut found)?;
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

/// Derive the canonical path for a region under `flavor`.
///
/// Only vanilla namespaces are derivable; hashed custom dimensions are
/// one-way, so their missing files surface [`WorldError::UnknownRegionPath`].
/// Rollback prefers discovered folders for those and only derives when the
/// file is absent - and must prefer same-dimension siblings over flavor
/// derivation whenever any exist, since folders may have moved since the
/// backup (e.g. across a 26.1 migration).
pub(crate) fn derive_path(
    world: impl AsRef<Path>,
    flavor: &LayoutFlavor,
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
    let kind_dir = kind_dir(kind).ok_or_else(unknown)?;
    let mut path = world.as_ref().to_path_buf();

    match flavor {
        LayoutFlavor::New => {
            let name = match dim {
                Dimension::OVERWORLD => "overworld",
                Dimension::NETHER => "the_nether",
                Dimension::END => "the_end",
                _ => return Err(unknown()),
            };
            path.push("dimensions");
            path.push("minecraft");
            path.push(name);
        }
        LayoutFlavor::Legacy => match dim {
            Dimension::OVERWORLD => {}
            Dimension::NETHER => path.push("DIM-1"),
            Dimension::END => path.push("DIM1"),
            _ => return Err(unknown()),
        },
        LayoutFlavor::Bukkit { base } => match dim {
            Dimension::OVERWORLD => path.push(base),
            Dimension::NETHER => {
                path.push(format!("{base}_nether"));
                path.push("DIM-1");
            }
            Dimension::END => {
                path.push(format!("{base}_the_end"));
                path.push("DIM1");
            }
            _ => return Err(unknown()),
        },
    }

    path.push(kind_dir);
    path.push(format!("r.{region_x}.{region_z}.mca"));
    Ok(path)
}

/// Path for a missing region inside a live sibling's directory, if any.
///
/// Folders may have moved since the backup (e.g. across a 26.1 migration),
/// so callers prefer this over flavor derivation whenever a same-dimension
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
