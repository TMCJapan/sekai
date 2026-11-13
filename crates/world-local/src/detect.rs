use std::{fs, path::Path};

use super::vanilla;
use crate::WorldError;

pub(super) enum Layout {
    Vanilla,
    VanillaLegacy,
    Bukkit(String),
    BukkitLegacy(String),
}

pub(super) fn detect(root: impl AsRef<Path>) -> Result<Option<Layout>, WorldError> {
    let root = root.as_ref();
    for base in bukkit_bases(root)? {
        if is_world_folder_any(&[
            root.join(&base)
                .join("dimensions")
                .join("minecraft")
                .join("overworld"),
            root.join(&base)
                .join("dimensions")
                .join("minecraft")
                .join("the_nether"),
            root.join(&base)
                .join("dimensions")
                .join("minecraft")
                .join("the_end"),
        ])? {
            return Ok(Some(Layout::Bukkit(base)));
        }

        if is_world_folder_any(&[
            root.join(&base),
            root.join(format!("{base}_nether")).join("DIM-1"),
            root.join(format!("{base}_the_end")).join("DIM1"),
        ])? {
            return Ok(Some(Layout::BukkitLegacy(base)));
        }
    }

    if is_world_folder_any(&[
        root.join("dimensions").join("minecraft").join("overworld"),
        root.join("dimensions").join("minecraft").join("the_nether"),
        root.join("dimensions").join("minecraft").join("the_end"),
    ])? {
        return Ok(Some(Layout::Vanilla));
    }

    if is_world_folder_any(&[root.to_path_buf(), root.join("DIM-1"), root.join("DIM1")])? {
        return Ok(Some(Layout::VanillaLegacy));
    }

    Ok(None)
}

/// Candidate Bukkit bases, most authoritative first.
///
/// A usable `level-name` is the only candidate - the server decides.
/// Without one (missing or unreadable `server.properties`, or an unusable
/// value), bases are elected from directory structure alone: a folder
/// nesting a `dimensions/` tree, or one with a `_nether`/`_the_end`
/// sibling world. The game default `world` wins ties; the rest keep
/// lexical order, so the pick is deterministic. Election only proposes
/// candidates - the world-folder checks below decide.
fn bukkit_bases(root: impl AsRef<Path>) -> Result<Vec<String>, WorldError> {
    let root = root.as_ref();
    if let Some(base) = level_name(root) {
        return Ok(vec![base]);
    }

    let mut bases = Vec::new();
    for entry in vanilla::read_dir_opt(root).map_err(|source| WorldError::io(root, source))? {
        if !vanilla::is_dir(&entry) {
            continue;
        }
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if matches!(name, "dimensions" | "DIM-1" | "DIM1") {
            continue; // the layout's own subdirectories, never a base
        }
        let path = entry.path();
        let sibling_trio = path.with_file_name(format!("{name}_nether")).is_dir()
            || path.with_file_name(format!("{name}_the_end")).is_dir();
        if sibling_trio || path.join("dimensions").is_dir() {
            bases.push(name.to_owned());
        }
    }
    bases.sort();
    if let Some(pos) = bases.iter().position(|base| base == "world") {
        bases.swap(0, pos);
    }
    Ok(bases)
}

/// `level-name` from a Bukkit/Paper `server.properties`, when readable.
///
/// The file is untrusted input: only uncommented `key=value` lines with
/// that exact key count, the value is trimmed of surrounding spaces, and
/// anything but one plain directory name is ignored like an unreadable
/// file - the layout must never follow a corrupted value out of the root.
fn level_name(root: impl AsRef<Path>) -> Option<String> {
    let root = root.as_ref();
    let text = fs::read_to_string(root.join("server.properties")).ok()?;
    text.lines().find_map(|line| {
        line.trim()
            .strip_prefix("level-name=")
            .map(str::trim)
            .filter(|value| is_plain_dir_name(value))
            .map(ToOwned::to_owned)
    })
}

/// Whether `value` is a single ordinary directory name: not empty, not
/// `.`/`..`, and no `/` or `\` - so no absolute spelling passes on any
/// supported platform. The value is joined onto the observed root, and a
/// single plain component can never leave it.
fn is_plain_dir_name(value: &str) -> bool {
    !value.is_empty() && value != "." && value != ".." && !value.contains(['/', '\\'])
}

/// Whether any candidate is a world folder, checked in order: the first
/// `true` wins, and a candidate that cannot be inspected fails the call -
/// silently answering `false` would misclassify the layout and open the
/// wrong tree.
fn is_world_folder_any(dirs: &[impl AsRef<Path>]) -> Result<bool, WorldError> {
    for dir in dirs {
        if is_world_folder(dir)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether `dir` is a world folder: one of `region/`, `entities/`, `poi/`
/// holds a parseable region file.
///
/// A missing candidate simply is not one; any other failure to inspect it
/// fails the call. Kind directories are listed like `vanilla::read_dir_opt`
/// (only `NotFound` means absent), and an unreadable entry is skipped the
/// same way discovery skips it, so one broken link cannot flip the layout.
fn is_world_folder(dir: impl AsRef<Path>) -> Result<bool, WorldError> {
    let dir = dir.as_ref();
    match fs::metadata(dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Ok(false),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(source) => return Err(WorldError::io(dir, source)),
    }
    for kind_dir in ["region", "entities", "poi"] {
        let path = dir.join(kind_dir);
        let entries =
            vanilla::read_dir_opt(&path).map_err(|source| WorldError::io(&path, source))?;
        for entry in entries {
            if !vanilla::is_file(&entry) {
                continue;
            }
            let filename = entry.file_name();
            let Some(name) = filename.to_str() else {
                continue;
            };
            if sekai_anvil::parse_region_name(name).is_ok() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::is_plain_dir_name;

    #[test]
    fn only_plain_directory_names_pass() {
        assert!(is_plain_dir_name("world"));
        assert!(is_plain_dir_name("my world"));
        assert!(is_plain_dir_name("world..2"));
        assert!(!is_plain_dir_name(""));
        assert!(!is_plain_dir_name("."));
        assert!(!is_plain_dir_name(".."));
        assert!(!is_plain_dir_name("../escape"));
        assert!(!is_plain_dir_name("nested/world"));
        assert!(!is_plain_dir_name("nested\\world"));
        assert!(!is_plain_dir_name("/absolute"));
    }
}
