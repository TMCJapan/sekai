//! Pluggable [`WorldTree`] implementations with their own failure types.
//!
//! Guards the abstraction boundary: a tree outside this workspace names its
//! own error, and the `sekai-app` entry points convert it via `From`.

use std::path::PathBuf;

use sekai_app::{AppError, Dimension, RegionKey, RegionKind, WorldTree, world_chunk_coords};

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

/// A tree that enumerates one dimension and serves no regions, with a
/// switch failing either discovery call.
#[derive(Clone)]
struct FakeWorld {
    fail_dims: bool,
    fail_regions: bool,
}

impl FakeWorld {
    const fn check(fail: bool) -> Result<(), FakeError> {
        if fail {
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
        Ok(true)
    }

    fn get_dims(&self) -> Result<Vec<Dimension>, Self::Error> {
        Self::check(self.fail_dims).map(|()| vec![Dimension::OVERWORLD])
    }

    fn get_regions(
        &self,
        _dim: Dimension,
        _kind: RegionKind,
    ) -> Result<Vec<(RegionKey, PathBuf)>, Self::Error> {
        Self::check(self.fail_regions).map(|()| Vec::new())
    }

    fn derive_path(&self, _key: RegionKey) -> Result<PathBuf, Self::Error> {
        Ok(PathBuf::from("/fake/region/r.0.0.mca"))
    }
}

#[test]
fn custom_tree_serves_the_app_layer() {
    // Enumeration splits across `get_dims` and `get_regions`: the fake lists
    // one empty dimension, and the app layer walks both calls.
    let world = FakeWorld {
        fail_dims: false,
        fail_regions: false,
    };
    assert_eq!(
        world_chunk_coords(&world).unwrap(),
        [] as [sekai_app::ChunkCoord; 0]
    );
}

#[test]
fn custom_tree_error_crosses_the_app_boundary() {
    let error = world_chunk_coords(&FakeWorld {
        fail_dims: true,
        fail_regions: false,
    })
    .unwrap_err();
    assert!(error.to_string().contains("fake world failed: unplugged"));

    // `get_regions` fails through the same boundary, not only `get_dims`:
    // `get_dims` succeeds and the app layer forwards its dimension.
    let error = world_chunk_coords(&FakeWorld {
        fail_dims: false,
        fail_regions: true,
    })
    .unwrap_err();
    assert!(error.to_string().contains("fake world failed: unplugged"));
}
