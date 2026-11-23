//! Pluggable [`WorldTree`] implementations with their own failure causes.
//!
//! Guards the abstraction boundary: a tree outside this workspace pins its
//! `Error` to [`WorldError`], wraps its own causes in [`WorldError::Io`],
//! hands out its own region handles, and the `sekai-app` entry points carry
//! the failure unchanged. Nothing in the
//! boundary is filesystem-bound: this tree only names its region files, and
//! every file operation on them fails on purpose - the fake serves no bytes.

use sekai_app::{
    AppError, Dimension, Region, RegionFile, RegionKey, RegionKind, UnguardedReason, WorldError,
    WorldLock, WorldLockMode, WorldLockState, WorldTree, world_chunk_coords,
};

/// Cause of a [`FakeWorld`] failure, deliberately unrelated to any host error.
#[derive(Debug, thiserror::Error)]
#[error("fake world failed: {reason}")]
struct FakeError {
    reason: &'static str,
}

/// Wrap a fake cause the way any tree outside this workspace wraps its own.
fn fake_failure(location: &str, reason: &'static str) -> WorldError {
    WorldError::io(location, FakeError { reason })
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
    fn check(fail: bool) -> Result<(), WorldError> {
        if fail {
            Err(fake_failure("/fake", "unplugged"))
        } else {
            Ok(())
        }
    }
}

/// Image of [`FakeRegion`]. [`FakeRegion::read`] always fails, so no
/// method below is reachable; the type exists only because `RegionFile`
/// names one.
#[derive(Clone, Debug)]
struct FakeRegionImage;

impl Region for FakeRegionImage {
    type Error = WorldError;

    fn image(&self) -> &[u8] {
        unreachable!("fake image serves no bytes")
    }

    fn visit_chunks<F>(&self, _visit: F) -> Result<(), WorldError>
    where
        F: FnMut(sekai_anvil::Chunk<'_>) -> bool,
    {
        unreachable!("fake image serves no chunks")
    }

    fn chunk_payload(&self, _x: i32, _z: i32) -> Result<Option<&[u8]>, WorldError> {
        unreachable!("fake image serves no chunks")
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
    type Error = WorldError;

    type RegionImage = FakeRegionImage;

    fn read(&self, _key: RegionKey) -> Result<Self::RegionImage, WorldError> {
        Err(fake_failure(&self.name, "no bytes"))
    }

    fn size(&self) -> Result<u64, WorldError> {
        // Not supported: the fake holds no files.
        Err(fake_failure(&self.name, "no size"))
    }

    fn fingerprint(&self, _key: RegionKey) -> Result<sekai_app::RegionFingerprint, WorldError> {
        // Not supported: the fake serves no bytes to fingerprint.
        Err(fake_failure(&self.name, "no fingerprint"))
    }

    fn exists(&self) -> bool {
        // Not supported: the fake holds no files.
        false
    }

    fn write(&self, _image: &[u8]) -> Result<(), WorldError> {
        // Not supported: the fake holds no files.
        Err(fake_failure(&self.name, "no writes"))
    }

    fn remove(&self) -> Result<(), WorldError> {
        // Not supported: the fake holds no files.
        Err(fake_failure(&self.name, "no removals"))
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
    type Error = WorldError;

    type Region = FakeRegion;
    type Lock = FakeLock;

    fn get_dims(&self) -> Result<Vec<Dimension>, WorldError> {
        Self::check(self.fail_dims).map(|()| vec![Dimension::OVERWORLD])
    }

    fn get_regions(
        &self,
        dim: Dimension,
        kind: RegionKind,
    ) -> Result<Vec<(RegionKey, Self::Region)>, WorldError> {
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

    fn derive_region(&self, key: RegionKey) -> Result<Self::Region, WorldError> {
        Ok(FakeRegion {
            name: fake_name(key),
        })
    }

    fn sibling_region(
        &self,
        _region: &Self::Region,
        key: RegionKey,
    ) -> Result<Self::Region, WorldError> {
        Ok(FakeRegion {
            name: fake_name(key),
        })
    }

    fn lock(&self, _mode: WorldLockMode) -> Result<WorldLockState<Self::Lock>, WorldError> {
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
    assert!(matches!(error, AppError::World(WorldError::Io { .. })));
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

/// Only the files a plan ingests are read: an out-of-scope file whose
/// `read` fails cannot fail the run, while the same failure in scope does.
#[tokio::test]
async fn only_planned_files_are_read() {
    use sekai_storage_local::LocalStore;

    let root = tempfile::Builder::new()
        .prefix("sekai-world-tree-planned-")
        .tempdir()
        .unwrap();
    let store = root.path().join("store").to_string_lossy().into_owned();
    let instance = sekai_app::SekaiInstance::<LocalStore>::init(&store)
        .await
        .unwrap();
    // The fake serves overworld files only, and every `read` fails.
    let world = FakeWorld {
        fail_dims: false,
        fail_regions: false,
        serve_region: true,
    };
    let options = sekai_app::StatusOptions {};

    let (report, _) = instance
        .world(&world)
        .status(options, sekai_app::Scope::dimension(Dimension::NETHER), &())
        .await
        .unwrap();
    assert!(report.clean);

    let error = instance
        .world(&world)
        .status(options, sekai_app::Scope::World, &())
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("fake world failed: no bytes"),
        "unexpected error: {error}"
    );
}
