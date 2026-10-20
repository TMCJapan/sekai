//! Abstract world access boundary.

mod hostworld;

use sekai_core::{Dimension, RegionKey, RegionKind};

use crate::region::RegionFile;

pub use hostworld::{HostRegion, HostWorldTree};

/// World access shared by backup, export, rollback, and diff.
///
/// Bundles region discovery and handle derivation so `sekai-app` operations
/// can accept any world implementation. Handles are cheap to clone: blocking
/// operations run on a blocking pool, and a borrow cannot cross
/// `spawn_blocking`, so callers clone the handle into the task.
///
/// A tree owns the layout: it names the region files it holds and derives
/// the ones that are missing. What happens to a file is the tree's business
/// too, behind the [`RegionFile`] handle - a host tree reads and writes the
/// filesystem, another tree may read and write something else.
///
/// The associated [`WorldTree::Error`] keeps the trait implementable outside
/// this crate: consumers choose their own failure type and convert it at
/// their boundary - `sekai-app` operations require
/// `AppError: From<WorldTree::Error>`.
pub trait WorldTree: Sync + Send + Clone + 'static {
    /// Failure type for every operation on this tree.
    type Error: std::error::Error;

    /// Handle to one region file of this tree.
    type Region: RegionFile<Error = Self::Error>;

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

    /// Every region file of `kind` inside `dim` as its identity and a
    /// handle to it, ordered by coordinates.
    ///
    /// The handle is the file discovery observed, not a layout-derived
    /// spelling: it may point at a non-canonical name (`r.00.00.mca`). One
    /// entry per coordinate is reported; two files spelling the same
    /// coordinate are ambiguous and must fail rather than silently drop one
    /// of them. Reporting one identity twice breaks that contract, and
    /// callers reject it as a broken tree.
    ///
    /// A `dim` that is not part of this tree (not returned by
    /// [`WorldTree::get_dims`]) yields an empty vector, not an error. The
    /// same holds for a kind directory that is absent or was removed.
    fn get_regions(
        &self,
        dim: Dimension,
        kind: RegionKind,
    ) -> Result<Vec<(RegionKey, Self::Region)>, Self::Error>;

    /// Layout-derived handle of one region file, whether or not it exists
    /// yet (export targets, restore targets for missing files).
    ///
    /// The name is canonical (`r.<x>.<z>.mca`). A world that holds a
    /// non-canonical spelling of the same region must be restored through
    /// that file's own handle: deriving the name here would create a
    /// second file for one region.
    ///
    /// Output trees may point at roots that do not exist before export, so
    /// this never touches the filesystem: the dimension only has to be part
    /// of the layout. Writing to the handle creates the directories the
    /// file needs.
    fn derive_region(&self, key: RegionKey) -> Result<Self::Region, Self::Error>;

    /// Handle of the region file that sits next to `region` in the same
    /// directory, named after `key`.
    ///
    /// `key` is expected to share `region`'s `dim` and `kind`: the
    /// directory is inherited from `region`, and only the file name comes
    /// from `key`.
    ///
    /// Folders may have moved since the backup, so a restore prefers this
    /// over the layout-derived handle: the live directory is what the
    /// sibling was observed in, while the derived handle names the
    /// layout's canonical directory.
    fn sibling_region(
        &self,
        region: &Self::Region,
        key: RegionKey,
    ) -> Result<Self::Region, Self::Error>;
}
