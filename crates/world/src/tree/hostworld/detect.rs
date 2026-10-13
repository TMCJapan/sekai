use std::{fs, path::Path};

pub(super) enum Layout {
    Vanilla,
    VanillaLegacy,
    Bukkit(String),
    BukkitLegacy(String),
}

pub(super) fn detect(root: impl AsRef<Path>) -> Option<Layout> {
    let root = root.as_ref();
    let text = fs::read_to_string(root.join("server.properties")).ok()?;
    let base = text.lines().find_map(|line| {
        line.trim()
            .strip_prefix("level-name=")
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })?;

    if is_world_folder_any(&[
        root.join(base)
            .join("dimensions")
            .join("minecraft")
            .join("overworld"),
        root.join(base)
            .join("dimensions")
            .join("minecraft")
            .join("nether"),
        root.join(base)
            .join("dimensions")
            .join("minecraft")
            .join("the_end"),
    ]) {
        return Some(Layout::Bukkit(base.to_owned()));
    }

    if is_world_folder_any(&[
        root.join(base),
        root.join(format!("{base}_nether")).join("DIM-1"),
        root.join(format!("{base}_the_end")).join("DIM1"),
    ]) {
        return Some(Layout::BukkitLegacy(base.to_owned()));
    }

    if is_world_folder_any(&[
        root.join("dimensions").join("minecraft").join("overworld"),
        root.join("dimensions").join("minecraft").join("nether"),
        root.join("dimensions").join("minecraft").join("the_end"),
    ]) {
        return Some(Layout::Vanilla);
    }

    if is_world_folder_any(&[root.to_path_buf(), root.join("DIM-1"), root.join("DIM1")]) {
        return Some(Layout::VanillaLegacy);
    }

    None
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
        for entry in std::fs::read_dir(dir.join(kind_dir))? {
            let entry = entry?;
            if !entry.metadata()?.is_file() {
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
