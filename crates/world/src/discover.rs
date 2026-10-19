//! Movement-tolerant region path lookup.
//!
//! Layout detection and dimension collection live in `tree::hostworld`:
//! opening a tree classifies the root once, collects one directory per
//! observed dimension, and hashes each dimension's canonical
//! `<namespace>:<name>` key into a stable `Dimension` id - the vanilla
//! trio included - so a world keeps its ids when it moves between layouts.
//! This module keeps the sibling lookup for files whose folders moved.

use std::collections::BTreeMap;
use std::path::PathBuf;

use sekai_core::RegionKey;

/// Path for a missing region inside a live sibling's directory, if any.
///
/// Folders may have moved since the backup (e.g. across a 26.1 migration),
/// so callers prefer this over derived paths whenever a same-dimension
/// sibling exists.
///
/// `discovered` must hold the paths discovery observed: the sibling's
/// *directory* is what this reuses, so derived paths would point the
/// lookup at the layout's canonical directory instead of the live one.
pub fn sibling_path(discovered: &BTreeMap<RegionKey, PathBuf>, key: &RegionKey) -> Option<PathBuf> {
    let sibling = discovered
        .iter()
        .find(|(k, _)| k.dim == key.dim && k.kind == key.kind)
        .map(|(_, path)| path)?;
    let mut path = sibling.clone();
    path.set_file_name(format!("r.{}.{}.mca", key.rx, key.rz));
    Some(path)
}
