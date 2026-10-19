//! Abstract world access boundary.

mod hostworld;

use std::path::PathBuf;

use sekai_core::{Dimension, RegionKey, RegionKind};

pub use hostworld::HostWorldTree;

/// World access shared by backup, export, rollback, and diff.
///
/// Bundles region discovery and path derivation so `sekai-app` operations
/// can accept any world implementation. Handles are cheap to clone: blocking
/// operations run on a blocking pool, and a borrow cannot cross
/// `spawn_blocking`, so callers clone the handle into the task.
///
/// The associated [`WorldTree::Error`] keeps the trait implementable outside
/// this crate: consumers choose their own failure type and convert it at
/// their boundary - `sekai-app` operations require
/// `AppError: From<WorldTree::Error>`.
pub trait WorldTree: Sync + Send + Clone + 'static {
    /// Failure type for every operation on this tree.
    type Error: std::error::Error;

    fn is_empty(&self) -> Result<bool, Self::Error>;

    /// Every dimension that may hold regions, ordered by ascending
    /// [`Dimension::raw`] code.
    ///
    /// Dimensions that belong to the tree but hold no region file yet are
    /// still listed; only dimensions outside the tree's layout are omitted.
    ///
    /// Raw codes are hashes of the canonical dimension ids, so this is not
    /// the trio order (`OVERWORLD`, `NETHER`, `END`) that the previous
    /// combined discovery yielded; per-dimension walks (backup, diff,
    /// rollback) visit dimensions in raw-code order instead.
    fn get_dims(&self) -> Result<Vec<Dimension>, Self::Error>;

    /// Every region file of `kind` inside `dim` as its identity and the
    /// actual on-disk path, ordered by coordinates.
    ///
    /// The path is the file discovery observed, not a layout-derived
    /// spelling: it may be non-canonical (`r.00.00.mca`). One entry per
    /// coordinate is reported; alternate spellings of the same coordinate
    /// are deduplicated by the implementation. Reporting one identity
    /// twice breaks that contract, and callers reject it as a broken tree
    /// instead of silently dropping a file.
    ///
    /// A `dim` that is not part of this tree (not returned by
    /// [`WorldTree::get_dims`]) yields an empty vector, not an error. The
    /// same holds for a kind directory that is absent or was removed.
    fn get_regions(
        &self,
        dim: Dimension,
        kind: RegionKind,
    ) -> Result<Vec<(RegionKey, PathBuf)>, Self::Error>;

    /// Layout-derived path of one region file, whether or not it exists on
    /// disk yet (export targets, restore targets for missing files).
    ///
    /// Output trees may point at roots that do not exist before export, so
    /// this never touches the filesystem: the dimension only has to be part
    /// of the layout. Callers writing to the result create parent
    /// directories.
    fn derive_path(&self, key: RegionKey) -> Result<PathBuf, Self::Error>;
}
