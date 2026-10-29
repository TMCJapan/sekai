//! End-to-end named scope flows over SQLite stores.
//!
//! Requires the `backend-sqlite` feature.

#![cfg(feature = "backend-sqlite")]

use sekai_app::{
    AppError, Area, ChunkCoord, Dimension, HostWorldTree, NamedScopeError, Rect, RegionKind, Scope,
    ScopeName,
};
use std::path::Path;

/// A private temp dir, removed on drop. Nothing is kept, so a panicking
/// test leaves nothing behind either.
fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("sekai-scope-")
        .tempdir()
        .unwrap()
}
const OVER: Dimension = Dimension::OVERWORLD;

fn write_region(path: impl AsRef<Path>, chunks: &[(i32, i32, Vec<u8>)]) {
    let name = path.as_ref().file_name().unwrap().to_str().unwrap();
    let (rx, rz) = sekai_anvil::parse_region_name(name).unwrap();
    let mut builder = sekai_anvil::RegionBuilder::new(rx, rz, 0).unwrap();
    for (x, z, payload) in chunks {
        builder.stage_chunk(*x, *z, payload).unwrap();
    }
    let image = builder.image().unwrap();
    std::fs::create_dir_all(path.as_ref().parent().unwrap()).unwrap();
    std::fs::write(path, image).unwrap();
}

fn name(raw: &str) -> ScopeName {
    ScopeName::parse(raw).unwrap()
}

#[tokio::test]
async fn a_scope_is_created_listed_and_deleted() {
    let root = tempdir();
    let mut instance = sekai_app::SekaiInstance::init(&root.path().join("store").to_string_lossy())
        .await
        .unwrap();
    assert_eq!(instance.list_scopes().await.unwrap(), Vec::new());

    let spawn = name("spawn");
    let created = instance
        .create_scope(
            &spawn,
            vec![(OVER, Rect::new(3, 3, 0, 0)), (OVER, Rect::new(0, 0, 3, 3))],
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        created.areas(),
        &[(OVER, Rect::new(0, 0, 3, 3))],
        "identical areas collapse"
    );
    let hub = instance
        .create_scope(&name("hub"), vec![(OVER, Rect::region(0, 0))], false)
        .await
        .unwrap();

    assert_eq!(
        instance.list_scopes().await.unwrap(),
        vec![hub, created],
        "listed whole, in name order"
    );

    assert!(instance.delete_scope(&spawn).await.unwrap());
    assert!(!instance.delete_scope(&spawn).await.unwrap());
    assert_eq!(instance.list_scopes().await.unwrap().len(), 1);
}

#[tokio::test]
async fn an_existing_scope_is_replaced_only_with_force() {
    let root = tempdir();
    let mut instance = sekai_app::SekaiInstance::init(&root.path().join("store").to_string_lossy())
        .await
        .unwrap();
    let spawn = name("spawn");
    let first = instance
        .create_scope(&spawn, vec![(OVER, Rect::new(0, 0, 0, 0))], false)
        .await
        .unwrap();

    let err = instance
        .create_scope(&spawn, vec![(OVER, Rect::new(1, 1, 1, 1))], false)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::ScopeExists { name } if name == "spawn"));
    assert_eq!(instance.list_scopes().await.unwrap(), vec![first.clone()]);

    let replaced = instance
        .create_scope(&spawn, vec![(OVER, Rect::new(1, 1, 1, 1))], true)
        .await
        .unwrap();
    assert_eq!(replaced.created_at_ms, first.created_at_ms);
    assert_eq!(replaced.areas(), [(OVER, Rect::new(1, 1, 1, 1))]);
    assert_eq!(instance.list_scopes().await.unwrap(), vec![replaced]);
}

#[tokio::test]
async fn a_selection_of_nothing_is_refused() {
    let root = tempdir();
    let mut instance = sekai_app::SekaiInstance::init(&root.path().join("store").to_string_lossy())
        .await
        .unwrap();
    let err = instance
        .create_scope(&name("spawn"), Vec::new(), false)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        AppError::InvalidScope(NamedScopeError::NoAreas)
    ));
    assert_eq!(instance.list_scopes().await.unwrap(), Vec::new());
}

#[tokio::test]
async fn a_scope_resolves_to_its_rectangles() {
    let root = tempdir();
    let mut instance = sekai_app::SekaiInstance::init(&root.path().join("store").to_string_lossy())
        .await
        .unwrap();
    let err = instance.resolve_scope(&name("spawn")).await.unwrap_err();
    assert!(matches!(err, AppError::UnknownScope { name } if name == "spawn"));

    instance
        .create_scope(
            &name("spawn"),
            vec![
                (OVER, Rect::region(0, 0)),
                (Dimension::NETHER, Rect::new(0, 0, 0, 0)),
            ],
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        instance.resolve_scope(&name("spawn")).await.unwrap(),
        Scope::Select {
            kinds: Scope::all_kinds(),
            areas: vec![
                (OVER, Area::Rect(Rect::region(0, 0))),
                (Dimension::NETHER, Area::Rect(Rect::new(0, 0, 0, 0))),
            ],
        }
    );
}

#[tokio::test]
async fn a_resolved_scope_limits_a_commit() {
    let root = tempdir();
    let world_path = root.path().join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, vec![1]), (1, 0, vec![2]), (5, 5, vec![3])],
    );
    let mut instance = sekai_app::SekaiInstance::init(&root.path().join("store").to_string_lossy())
        .await
        .unwrap();
    instance
        .create_scope(
            &name("spawn"),
            vec![(OVER, Rect::new(0, 0, 0, 0)), (OVER, Rect::new(5, 5, 5, 5))],
            false,
        )
        .await
        .unwrap();

    let scope = instance.resolve_scope(&name("spawn")).await.unwrap();
    let (report, _) = instance
        .world_mut(&mut world)
        .commit(sekai_app::CommitOptions::default(), scope, |_| {})
        .await
        .unwrap();
    let mut coords = instance.commit_chunk_coords(report.commit).await.unwrap();
    coords.sort_unstable();
    assert_eq!(
        coords,
        [
            ChunkCoord::new(OVER, RegionKind::Region, 0, 0),
            ChunkCoord::new(OVER, RegionKind::Region, 5, 5),
        ],
        "only the scope's two chunks are committed"
    );
}
