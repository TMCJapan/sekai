//! Abstract world access boundary.

mod hostworld;

use std::path::PathBuf;

use sekai_core::{Dimension, RegionKind};

use crate::RegionRef;

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
    fn discover(&self) -> Result<Vec<RegionRef>, Self::Error>;
    fn derive_path(
        &self,
        dim: Dimension,
        kind: RegionKind,
        region_x: i32,
        region_z: i32,
    ) -> Result<PathBuf, Self::Error>;
}
