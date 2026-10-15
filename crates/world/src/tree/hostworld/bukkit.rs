//! Pre-26.1 Bukkit split-layout dimension directories.
//!
//! CraftBukkit/Spigot/Paper anchor every dimension in its own world folder:
//! `validateAndCreateAccess(<name>, <stem>)` uses `<name> = level-name` for
//! the overworld, `<level-name>_nether`/`<level-name>_the_end` for the
//! default pair, and `<level-name>_<namespace>_<path>` for a datapack
//! dimension `<namespace>:<path>`. Only the default pair nests its
//! dimension root (`DIM-1`, `DIM1`); a custom dimension nests
//! `dimensions/<namespace>/<path>` inside its world folder
//! (`LevelStorageSource.getStorageFolder`). 26.1 later flattens everything
//! into the vanilla `dimensions/` layout.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use sekai_anvil::custom_dimension_id;
use sekai_core::Dimension;

use super::vanilla::{self, is_dir, read_dir_opt};
use crate::WorldError;

/// Fixed overworld/nether/end directories of the split layout around `base`.
///
/// Ids hash the same canonical names as the single-folder layout so a world
/// keeps its history when it moves between vanilla and Bukkit trees.
pub(super) fn fixed_dirs(root: &Path, base: &str) -> [(Dimension, PathBuf); 3] {
    [
        (custom_dimension_id("minecraft:overworld"), root.join(base)),
        (
            custom_dimension_id("minecraft:nether"),
            root.join(format!("{base}_nether")).join("DIM-1"),
        ),
        (
            custom_dimension_id("minecraft:the_end"),
            root.join(format!("{base}_the_end")).join("DIM1"),
        ),
    ]
}

/// Datapack dimension directories: every sibling world folder
/// `<base>_<namespace>_<path>` that holds a `dimensions/` tree.
///
/// The folder name is ambiguous (`<namespace>` and `<path>` may both contain
/// `_`), so the dimension identity comes from the `dimensions/` tree itself.
/// A world whose data still sits in the main world's `dimensions/` folder is
/// pre-migration vanilla residue: the server moves it into a sibling folder
/// at startup and reads only the moved location.
pub(super) fn collect_custom_dimensions(
    root: &Path,
    base: &str,
) -> Result<BTreeMap<Dimension, PathBuf>, WorldError> {
    let prefix = format!("{base}_");
    let mut dims = BTreeMap::new();
    for entry in read_dir_opt(root).map_err(|source| WorldError::io(root, source))? {
        if !is_dir(&entry) {
            continue;
        }
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        let Some(world_type) = name.strip_prefix(&prefix) else {
            continue;
        };
        if world_type == "nether" || world_type == "the_end" {
            continue; // the fixed pair, whose data lives in DIM-1/DIM1
        }

        let dir = entry.path().join("dimensions");
        dims.extend(
            vanilla::collect_dimensions(&dir).map_err(|source| WorldError::io(&dir, source))?,
        );
    }
    Ok(dims)
}
