//! Region discovery shared by the world operations.

use alloc::collections::BTreeMap;
use alloc::collections::btree_map::Entry;

use sekai_util::{RegionKey, RegionKind};
use sekai_world::WorldTree;

use crate::error::{AppError, duplicate_region, world_error};

/// Every region file the tree reports, keyed by identity.
///
/// Commit, diff, and checkout walk the world the same way, so the walk
/// lives here once: the duplicate detection cannot drift between them.
pub(crate) fn discover_regions<T: WorldTree>(
    world: &T,
) -> Result<BTreeMap<RegionKey, T::Region>, AppError>
where
    T::Error: Send + Sync + 'static,
{
    let mut regions = BTreeMap::new();
    for dim in world.get_dims().map_err(world_error::<T>)? {
        for kind in RegionKind::ALL {
            for (key, region) in world.get_regions(dim, kind).map_err(world_error::<T>)? {
                match regions.entry(key) {
                    Entry::Vacant(entry) => {
                        entry.insert(region);
                    }
                    Entry::Occupied(entry) => {
                        return Err(duplicate_region(&key, entry.get(), &region));
                    }
                }
            }
        }
    }
    Ok(regions)
}
