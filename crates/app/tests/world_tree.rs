//! Pluggable [`WorldTree`] implementations with their own failure types.
//!
//! Guards the abstraction boundary: a tree outside this workspace names its
//! own error, hands out its own region handles, and the `sekai-app` entry
//! points convert the failure via `From`. Nothing in the boundary is
//! filesystem-bound: this tree only names its region files, and every file
//! operation on them fails on purpose - the fake serves no bytes.

use sekai_app::{
    AppError, Dimension, RegionFile, RegionKey, RegionKind, UnguardedReason, WorldLock,
    WorldLockMode, WorldLockState, WorldTree, world_chunk_coords,
};

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

/// A tree that enumerates one dimension, serves no listed regions but
/// derives region handles, with a switch failing either discovery call.
#[derive(Clone)]
struct FakeWorld {
    fail_dims: bool,
    fail_regions: bool,
    /// Report one region file, so the handle's own failures are exercised.
    serve_region: bool,
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

/// One region file of the fake tree: the name it was asked for, nothing
/// else.
#[derive(Clone, Debug)]
struct FakeRegion {
    name: String,
}

impl std::fmt::Display for FakeRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

impl RegionFile for FakeRegion {
    type Error = FakeError;

    fn read(&self) -> Result<Vec<u8>, Self::Error> {
        // Not supported: the fake serves no bytes.
        Err(FakeError { reason: "no bytes" })
    }

    fn fingerprint(&self, _key: RegionKey) -> Result<sekai_app::RegionFingerprint, Self::Error> {
        // Not supported: the fake serves no bytes to fingerprint.
        Err(FakeError {
            reason: "no fingerprint",
        })
    }

    fn exists(&self) -> bool {
        // Not supported: the fake holds no files.
        false
    }

    fn write(&self, _image: &[u8]) -> Result<(), Self::Error> {
        // Not supported: the fake holds no files.
        Err(FakeError {
            reason: "no writes",
        })
    }

    fn remove(&self) -> Result<(), Self::Error> {
        // Not supported: the fake holds no files.
        Err(FakeError {
            reason: "no removals",
        })
    }
}

/// Guard type of [`FakeWorld`].
///
/// The fake excludes nothing, so it never hands this out; the type only
/// exists because the trait names one.
#[derive(Debug)]
struct FakeLock;

impl WorldLock for FakeLock {
    fn mode(&self) -> WorldLockMode {
        WorldLockMode::Shared
    }
}

impl WorldTree for FakeWorld {
    type Error = FakeError;
    type Region = FakeRegion;
    type Lock = FakeLock;

    fn is_empty(&self) -> Result<bool, Self::Error> {
        Ok(true)
    }

    fn get_dims(&self) -> Result<Vec<Dimension>, Self::Error> {
        Self::check(self.fail_dims).map(|()| vec![Dimension::OVERWORLD])
    }

    fn get_regions(
        &self,
        dim: Dimension,
        kind: RegionKind,
    ) -> Result<Vec<(RegionKey, Self::Region)>, Self::Error> {
        Self::check(self.fail_regions)?;
        if !self.serve_region {
            return Ok(Vec::new());
        }
        let key = RegionKey::new(dim, kind, 0, 0);
        Ok(vec![(
            key,
            FakeRegion {
                name: fake_name(key),
            },
        )])
    }

    fn derive_region(&self, key: RegionKey) -> Result<Self::Region, Self::Error> {
        Ok(FakeRegion {
            name: fake_name(key),
        })
    }

    fn sibling_region(
        &self,
        _region: &Self::Region,
        key: RegionKey,
    ) -> Result<Self::Region, Self::Error> {
        Ok(FakeRegion {
            name: fake_name(key),
        })
    }

    fn lock(&self, _mode: WorldLockMode) -> Result<WorldLockState<Self::Lock>, Self::Error> {
        // Nothing to exclude: the fake holds no files. The reason is
        // stated, so a caller can see that it runs unguarded.
        Ok(WorldLockState::Unguarded(UnguardedReason::NothingToProtect))
    }
}

/// The name the fake gives the region file of `key`.
fn fake_name(key: RegionKey) -> String {
    format!("/fake/region/r.{}.{}.mca", key.rx, key.rz)
}

#[test]
fn custom_tree_serves_the_app_layer() {
    // Enumeration splits across `get_dims` and `get_regions`: the fake lists
    // one empty dimension, and the app layer walks both calls.
    let world = FakeWorld {
        fail_dims: false,
        fail_regions: false,
        serve_region: false,
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
        serve_region: false,
    })
    .unwrap_err();
    assert!(error.to_string().contains("fake world failed: unplugged"));

    // `get_regions` fails through the same boundary, not only `get_dims`:
    // `get_dims` succeeds and the app layer forwards its dimension.
    let error = world_chunk_coords(&FakeWorld {
        fail_dims: false,
        fail_regions: true,
        serve_region: false,
    })
    .unwrap_err();
    assert!(error.to_string().contains("fake world failed: unplugged"));
}

/// A region handle carries its own failure, and that one crosses the app
/// boundary too - not only the tree's discovery calls.
#[test]
fn region_handle_error_crosses_the_app_boundary() {
    let error = world_chunk_coords(&FakeWorld {
        fail_dims: false,
        fail_regions: false,
        serve_region: true,
    })
    .unwrap_err();
    assert!(
        error.to_string().contains("fake world failed: no bytes"),
        "unexpected error: {error}"
    );
}

/// The tree states that it excludes nothing, and says why.
#[test]
fn custom_tree_reports_its_unguarded_answer() {
    let world = FakeWorld {
        fail_dims: false,
        fail_regions: false,
        serve_region: false,
    };
    for mode in [WorldLockMode::Shared, WorldLockMode::Exclusive] {
        let state = world.lock(mode).unwrap();
        assert!(!state.is_guarded(), "{mode:?}");
        assert_eq!(
            state.unguarded_reason(),
            Some(UnguardedReason::NothingToProtect),
            "{mode:?}"
        );
        assert_eq!(
            state.guard().unwrap_err(),
            UnguardedReason::NothingToProtect,
            "{mode:?}"
        );
    }
}
