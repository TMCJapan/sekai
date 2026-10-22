//! Abstract world access boundary.

mod hostworld;

use sekai_core::{Dimension, RegionKey, RegionKind};

use crate::region::RegionFile;
use crate::{WorldLock, WorldLockMode, WorldLockState};

pub use hostworld::{HostRegion, HostWorldLock, HostWorldLockError, HostWorldTree};

/// World access shared by commit, clone, checkout, and diff.
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

    /// Guard [`WorldTree::lock`] hands out.
    type Lock: WorldLock;

    fn is_empty(&self) -> Result<bool, Self::Error>;

    /// Every dimension that may hold regions, ordered by ascending
    /// [`Dimension::raw`] code.
    ///
    /// Dimensions that belong to the tree but hold no region file yet are
    /// still listed; only dimensions outside the tree's layout are omitted.
    ///
    /// Raw codes are hashes of the canonical dimension ids, so this is not
    /// the trio order (`OVERWORLD`, `NETHER`, `END`) that the previous
    /// combined discovery yielded; per-dimension walks (commit, diff,
    /// checkout) visit dimensions in raw-code order instead.
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
    /// yet (clone targets, checkout targets for missing files).
    ///
    /// The name is canonical (`r.<x>.<z>.mca`). A world that holds a
    /// non-canonical spelling of the same region must be restored through
    /// that file's own handle: deriving the name here would create a
    /// second file for one region.
    ///
    /// Output trees may point at roots that do not exist before clone, so
    /// this never touches the filesystem: the dimension only has to be part
    /// of the layout. Writing to the handle creates the directories the
    /// file needs.
    fn derive_region(&self, key: RegionKey) -> Result<Self::Region, Self::Error>;

    /// Handle of the region file that sits next to `region` in the same
    /// directory, named after `key`.
    ///
    /// The caller must pass a `key` that shares `region`'s `dim` and
    /// `kind`: the directory is inherited from `region`, and only the file
    /// name comes from `key`. That directory is the one the region was
    /// observed in, so it exists.
    ///
    /// Folders may have moved since the commit, so a restore prefers this
    /// over the layout-derived handle: the live directory is what the
    /// sibling was observed in, while the derived handle names the
    /// layout's canonical directory.
    fn sibling_region(
        &self,
        region: &Self::Region,
        key: RegionKey,
    ) -> Result<Self::Region, Self::Error>;

    /// Lock the tree against other sekai operations in `mode`, held until
    /// the returned guard drops.
    ///
    /// The answer states whether the world is actually excluded:
    /// [`WorldLockState::Guarded`] carries the guard, while
    /// [`WorldLockState::Unguarded`] names the reason the operation runs
    /// without exclusion. Every tree has to decide that explicitly: a
    /// silent unguarded default would let a checkout overlap a commit
    /// without warning. Contention surfaces as its own failure so callers
    /// can tell "busy, retry" from a real one; [`HostWorldTree`] reports
    /// [`crate::WorldError::WorldLocked`].
    ///
    /// A tree is locked through the root it was constructed from: the
    /// guard excludes other operations that lock that same root. Two
    /// roots naming the same world hold two different locks and do not
    /// exclude each other, so a caller that names one world through
    /// several roots has to pick one canonical root.
    ///
    /// [`WorldLockState::Unguarded`] means no exclusion at all: the
    /// operation runs while another operation may hold the same world.
    /// A writer must not treat that as protected - see
    /// [`WorldLockMode::Exclusive`], which never answers unguarded.
    fn lock(&self, mode: WorldLockMode) -> Result<WorldLockState<Self::Lock>, Self::Error>;
}
