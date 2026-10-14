use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use sekai_anvil::custom_dimension_id;
use sekai_core::Dimension;

pub(super) fn collect_dimentions(
    root: impl AsRef<Path>,
) -> std::io::Result<BTreeMap<Dimension, PathBuf>> {
    let mut dims = BTreeMap::new();
    for namespace in read_dir_opt(root)? {
        let ns_filename = namespace.file_name();
        for dimension in read_dir_opt(namespace.path())? {
            let dim_filename = dimension.file_name();
            let (Some(ns), Some(dim)) = (ns_filename.to_str(), dim_filename.to_str()) else {
                continue;
            };

            dims.insert(
                custom_dimension_id(&format!("{ns}:{dim}")),
                dimension.path(),
            );
        }
    }

    Ok(dims)
}

/// Directory entries, treating only a missing directory as empty. Other
/// I/O failures still surface: a dimension that cannot be listed must not
/// silently drop out of the tree.
fn read_dir_opt(dir: impl AsRef<Path>) -> std::io::Result<Vec<std::fs::DirEntry>> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries.collect(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(err),
    }
}
