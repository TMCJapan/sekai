//! A tree names its own failure type.
//!
//! Only `sekai-app` pins `WorldTree::Error` to `WorldError` (ADR-0040); the
//! boundary itself stays generic, so a consumer can run a tree that cannot
//! fail at all and match its results without an error arm.

use std::convert::Infallible;

use sekai_core::{Dimension, RegionFingerprint, RegionKey, RegionKind};
use sekai_world::{
    Region, RegionFile, UnguardedReason, WorldLock, WorldLockMode, WorldLockState, WorldTree,
};

/// A tree that holds no region files and cannot fail.
#[derive(Clone, Debug)]
struct EmptyTree;

#[derive(Clone, Debug)]
struct EmptyRegion;

impl std::fmt::Display for EmptyRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("empty")
    }
}

struct EmptyImage;

impl Region for EmptyImage {
    type Error = Infallible;

    fn image(&self) -> &[u8] {
        &[]
    }

    fn visit_chunks<F>(&self, _visit: F) -> Result<(), Infallible>
    where
        F: FnMut(sekai_anvil::Chunk<'_>) -> bool,
    {
        Ok(())
    }

    fn chunk_payload(&self, _x: i32, _z: i32) -> Result<Option<&[u8]>, Infallible> {
        Ok(None)
    }
}

impl RegionFile for EmptyRegion {
    type Error = Infallible;

    type RegionImage = EmptyImage;

    fn read(&self, _key: RegionKey) -> Result<EmptyImage, Infallible> {
        Ok(EmptyImage)
    }

    fn size(&self) -> Result<u64, Infallible> {
        Ok(0)
    }

    fn fingerprint(&self, key: RegionKey) -> Result<RegionFingerprint, Infallible> {
        Ok(RegionFingerprint {
            key,
            mtime_ms: None,
            size: 0,
            content_hash: [0; 32],
        })
    }

    fn exists(&self) -> bool {
        false
    }

    fn write(&self, _image: &[u8]) -> Result<(), Infallible> {
        Ok(())
    }

    fn remove(&self) -> Result<(), Infallible> {
        Ok(())
    }
}

#[derive(Debug)]
struct NoLock;

impl WorldLock for NoLock {
    fn mode(&self) -> WorldLockMode {
        WorldLockMode::Shared
    }
}

impl WorldTree for EmptyTree {
    type Error = Infallible;

    type Region = EmptyRegion;

    type Lock = NoLock;

    fn get_dims(&self) -> Result<Vec<Dimension>, Infallible> {
        Ok(Vec::new())
    }

    fn get_regions(
        &self,
        _dim: Dimension,
        _kind: RegionKind,
    ) -> Result<Vec<(RegionKey, EmptyRegion)>, Infallible> {
        Ok(Vec::new())
    }

    fn derive_region(&self, _key: RegionKey) -> Result<EmptyRegion, Infallible> {
        Ok(EmptyRegion)
    }

    fn sibling_region(
        &self,
        _region: &EmptyRegion,
        _key: RegionKey,
    ) -> Result<EmptyRegion, Infallible> {
        Ok(EmptyRegion)
    }

    fn lock(&self, _mode: WorldLockMode) -> Result<WorldLockState<NoLock>, Infallible> {
        Ok(WorldLockState::Unguarded(UnguardedReason::NothingToProtect))
    }
}

#[test]
fn infallible_tree_needs_no_error_arm() {
    let tree = EmptyTree;
    let Ok(dims) = tree.get_dims();
    assert_eq!(dims, Vec::new());

    let key = RegionKey::new(Dimension::OVERWORLD, RegionKind::Region, 0, 0);
    let Ok(region) = tree.derive_region(key);
    let Ok(image) = region.read(key);
    let Ok(payload) = image.chunk_payload(0, 0);
    assert_eq!(payload, None);
}
