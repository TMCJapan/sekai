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
    for namespace in std::fs::read_dir(root)? {
        let namespace = namespace?;
        let ns_filename = namespace.file_name();
        for dimension in std::fs::read_dir(namespace.path())? {
            let dimension = dimension?;
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
