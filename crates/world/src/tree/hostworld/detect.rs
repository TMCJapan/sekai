use std::{fs, path::Path};

pub(super) enum Layout {
    Vanilla,
    VanillaLegacy,
    Bukkit(String),
    BukkitLegacy(String),
}

pub(super) fn detect(root: impl AsRef<Path>) -> Option<Layout> {
    let root = root.as_ref();
    if let Some(base) = level_name(root) {
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
        ]) {
            return Some(Layout::Bukkit(base));
        }

        if is_world_folder_any(&[
            root.join(&base),
            root.join(format!("{base}_nether")).join("DIM-1"),
            root.join(format!("{base}_the_end")).join("DIM1"),
        ]) {
            return Some(Layout::BukkitLegacy(base));
        }
    }

    if is_world_folder_any(&[
        root.join("dimensions").join("minecraft").join("overworld"),
        root.join("dimensions").join("minecraft").join("the_nether"),
        root.join("dimensions").join("minecraft").join("the_end"),
    ]) {
        return Some(Layout::Vanilla);
    }

    if is_world_folder_any(&[root.to_path_buf(), root.join("DIM-1"), root.join("DIM1")]) {
        return Some(Layout::VanillaLegacy);
    }

    None
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

fn is_world_folder_any(dirs: &[impl AsRef<Path>]) -> bool {
    dirs.iter()
        .map(is_world_folder)
        .any(|x| *x.as_ref().unwrap_or(&false))
}

fn is_world_folder(dir: impl AsRef<Path>) -> std::io::Result<bool> {
    let dir = dir.as_ref();
    if !dir.is_dir() {
        return Ok(false);
    }
    for kind_dir in ["region", "entities", "poi"] {
        let Ok(entries) = std::fs::read_dir(dir.join(kind_dir)) else {
            continue;
        };
        for entry in entries {
            let entry = entry?;
            if !std::fs::metadata(entry.path())?.is_file() {
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
