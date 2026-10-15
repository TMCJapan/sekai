use std::{
    collections::BTreeMap,
    ffi::OsStr,
    path::{Path, PathBuf},
};

use sekai_anvil::custom_dimension_id_bytes;
use sekai_core::Dimension;

pub(super) fn collect_dimensions(
    root: impl AsRef<Path>,
) -> std::io::Result<BTreeMap<Dimension, PathBuf>> {
    let mut dims = BTreeMap::new();
    for namespace in read_dir_opt(root)? {
        if !is_dir(&namespace) {
            continue;
        }
        let namespace_name = namespace.file_name();
        for dimension in read_dir_opt(namespace.path())? {
            if !is_dir(&dimension) {
                continue;
            }
            let key = canonical_key(&namespace_name, &dimension.file_name());
            dims.insert(custom_dimension_id_bytes(&key), dimension.path());
        }
    }

    Ok(dims)
}

/// Canonical `<namespace>:<name>` key of two directory names.
///
/// Directory names are arbitrary file-system bytes (`OsStr` encoding is a
/// self-synchronizing superset of UTF-8), so a name that is not valid UTF-8
/// still keys a stable id instead of dropping the dimension.
fn canonical_key(namespace: &OsStr, name: &OsStr) -> Vec<u8> {
    let mut key = namespace.as_encoded_bytes().to_vec();
    key.push(b':');
    key.extend_from_slice(name.as_encoded_bytes());
    key
}

/// Directory entries, treating only a missing directory as empty. Other
/// I/O failures still surface: a dimension that cannot be listed must not
/// silently drop out of the tree.
pub(super) fn read_dir_opt(dir: impl AsRef<Path>) -> std::io::Result<Vec<std::fs::DirEntry>> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries.collect(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(err),
    }
}

/// Whether the entry is a directory, following symlinks like the game
/// does; an unresolvable entry (dangling link, permission denied) is
/// skipped rather than failing the whole collection.
pub(super) fn is_dir(entry: &std::fs::DirEntry) -> bool {
    std::fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir())
}

/// Whether the entry is a regular file, following symlinks like the game
/// does; an unresolvable entry (dangling link, permission denied) is
/// skipped rather than failing the whole discovery.
pub(super) fn is_file(entry: &std::fs::DirEntry) -> bool {
    std::fs::metadata(entry.path()).is_ok_and(|meta| meta.is_file())
}
