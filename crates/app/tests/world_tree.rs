//! Pluggable [`WorldTree`] implementations with their own failure types.
//!
//! Guards the abstraction boundary: a tree outside this workspace names its
//! own error, and the `sekai-app` entry points convert it via `From`.

use std::path::PathBuf;

use sekai_app::{AppError, Dimension, RegionKind, WorldTree, world_chunk_coords};
use sekai_world::RegionRef;

/// Failure type of [`FakeWorld`], deliberately unrelated to `WorldError`.
#[derive(Debug, thiserror::Error)]
#[error("fake world failed: {reason}")]
struct FakeError {
    reason: &'static str,
}

impl From<FakeError> for AppError {
    fn from(source: FakeError) -> Self {
        Self::Io(std::io::Error::other(source))
    }
}

/// A tree serving no regions, with a switch for failing every operation.
#[derive(Clone)]
struct FakeWorld {
    fail: bool,
}

impl FakeWorld {
    const fn check(&self) -> Result<(), FakeError> {
        if self.fail {
            Err(FakeError {
                reason: "unplugged",
            })
        } else {
            Ok(())
        }
    }
}

impl WorldTree for FakeWorld {
    type Error = FakeError;

    fn is_empty(&self) -> Result<bool, Self::Error> {
        self.check().map(|()| true)
    }

    fn discover(&self) -> Result<Vec<RegionRef>, Self::Error> {
        self.check().map(|()| Vec::new())
    }

    fn derive_path(
        &self,
        _dim: Dimension,
        _kind: RegionKind,
        _region_x: i32,
        _region_z: i32,
    ) -> Result<PathBuf, Self::Error> {
        self.check()
            .map(|()| PathBuf::from("/fake/region/r.0.0.mca"))
    }
}

#[test]
fn custom_tree_serves_the_app_layer() {
    assert!(
        world_chunk_coords(&FakeWorld { fail: false })
            .unwrap()
            .is_empty()
    );
}

#[test]
fn custom_tree_error_crosses_the_app_boundary() {
    let error = world_chunk_coords(&FakeWorld { fail: true }).unwrap_err();
    assert!(error.to_string().contains("fake world failed: unplugged"));
}
