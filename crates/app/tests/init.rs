//! `init` and discovery over real world folders and local stores
//! (ADR-0035 *`init`*, *Identity and discovery*).
//!
//! Requires the `backend-local` feature.

#![cfg(feature = "backend-local")]

mod fixtures;

use fixtures::chunk_nbt;

use sekai_app::{
    AppError, HeadState, Initialized, Location, RefName, SEKAI_DIR, SekaiInstance, WorktreeName,
    WorldRoot, discover,
};
use std::path::{Path, PathBuf};

/// A private temp dir, removed on drop.
fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("sekai-init-")
        .tempdir()
        .unwrap()
}

/// A recognizable world at `root/name`: one region file with one chunk.
fn world(root: impl AsRef<Path>, name: &str) -> PathBuf {
    let world = root.as_ref().canonicalize().unwrap().join(name);
    let mut builder = sekai_anvil::RegionBuilder::new(0, 0, 0).unwrap();
    builder.stage_chunk(0, 0, &chunk_nbt("marker")).unwrap();
    std::fs::create_dir_all(world.join("region")).unwrap();
    std::fs::write(world.join("region/r.0.0.mca"), builder.image().unwrap()).unwrap();
    world
}

fn worktree(name: &str) -> WorktreeName {
    WorktreeName::parse(name).unwrap()
}

async fn init(world: impl AsRef<Path>) -> Result<Initialized, AppError> {
    SekaiInstance::init_world(world, None::<&Path>).await
}

async fn init_at(
    world: impl AsRef<Path>,
    store: impl AsRef<Path>,
) -> Result<Initialized, AppError> {
    SekaiInstance::init_world(world, Some(store)).await
}

#[tokio::test]
async fn init_makes_the_world_the_main_worktree() {
    let root = tempdir();
    let world = world(root.path(), "survival");
    let region = std::fs::read(world.join("region/r.0.0.mca")).unwrap();

    let initialized = init(&world).await.unwrap();
    assert!(initialized.created);
    assert_eq!(initialized.store, world.join(SEKAI_DIR));
    assert_eq!(initialized.instance.worktree(), Some(&worktree("survival")));
    assert_eq!(
        initialized.instance.head().await.unwrap(),
        Some(HeadState::Unborn {
            branch: RefName::parse("main").unwrap()
        })
    );
    // `init` leaves the world's files untouched.
    assert_eq!(
        std::fs::read(world.join("region/r.0.0.mca")).unwrap(),
        region
    );
    drop(initialized);

    // Discovery finds the main worktree from inside the world.
    let location = discover(world.join("region"), None::<&Path>).unwrap();
    assert_eq!(
        location,
        Location {
            store: world.join(SEKAI_DIR),
            world: Some(WorldRoot {
                root: world.clone(),
                linked: None
            }),
        }
    );
    let opened = SekaiInstance::open_location(&location).await.unwrap();
    assert_eq!(opened.worktree(), Some(&worktree("survival")));
    drop(opened);

    // Initializing again changes nothing.
    let again = init(&world).await.unwrap();
    assert!(!again.created);
    assert_eq!(again.instance.worktree(), Some(&worktree("survival")));
}

#[tokio::test]
async fn a_store_elsewhere_links_the_world() {
    let root = tempdir();
    let world = world(root.path(), "creative");
    let store = root.path().canonicalize().unwrap().join("store");

    let initialized = init_at(&world, &store).await.unwrap();
    assert!(initialized.created);
    assert_eq!(initialized.store, store);
    drop(initialized);
    assert_eq!(
        std::fs::read_to_string(world.join(SEKAI_DIR)).unwrap(),
        format!("store: {}\nworktree: creative\n", store.display())
    );

    let location = discover(&world, None::<&Path>).unwrap();
    assert_eq!(
        location,
        Location {
            store: store.clone(),
            world: Some(WorldRoot {
                root: world.clone(),
                linked: Some(worktree("creative"))
            }),
        }
    );
    let opened = SekaiInstance::open_location(&location).await.unwrap();
    assert!(opened.head().await.unwrap().is_some());
    drop(opened);

    assert!(!init_at(&world, &store).await.unwrap().created);
}

#[tokio::test]
async fn init_refuses_what_is_not_its_world() {
    let root = tempdir();

    // Not a world at all.
    let empty = root.path().canonicalize().unwrap().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    assert!(matches!(
        init(&empty).await,
        Err(AppError::NotAWorld { path }) if path == empty
    ));

    // An existing store lacking the world.
    let first = world(root.path(), "first");
    let store = root.path().join("store");
    drop(init_at(&first, &store).await.unwrap());
    let second = world(root.path(), "second");
    assert!(matches!(
        init_at(&second, &store).await,
        Err(AppError::StoreHasOtherWorld { .. })
    ));

    // A world whose `.sekai` names another store, linked or main.
    let other = root.path().join("other");
    assert!(matches!(
        init_at(&first, &other).await,
        Err(AppError::WorldInOtherStore { world, .. }) if world == first
    ));
    drop(init(&second).await.unwrap());
    assert!(matches!(
        init_at(&second, &other).await,
        Err(AppError::WorldInOtherStore { world, .. }) if world == second
    ));
    assert!(!other.exists(), "a refused init creates no store");
}
