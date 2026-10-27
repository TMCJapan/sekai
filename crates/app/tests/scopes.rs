//! End-to-end named scope flows over SQLite stores.
//!
//! Requires the `backend-sqlite` feature.

#![cfg(feature = "backend-sqlite")]

use sekai_app::{
    AppError, Area, ChunkCoord, Dimension, HostWorldTree, NamedScopeError, Rect, RegionKind, Scope,
    ScopeName,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

const OVER: Dimension = Dimension::OVERWORLD;

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "sekai-scope-{name}-{}-{}",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn store(&self) -> String {
        self.0.join("store").to_string_lossy().into_owned()
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

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
    let root = TempDir::new("crud");
    let mut instance = sekai_app::SekaiInstance::init(&root.store()).await.unwrap();
    assert_eq!(instance.list_scopes().await.unwrap(), Vec::new());

    let spawn = name("spawn");
    let created = instance
        .create_scope(
            &spawn,
            Some(vec![RegionKind::Region]),
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
        .create_scope(&name("hub"), None, vec![(OVER, Rect::region(0, 0))], false)
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
    let root = TempDir::new("force");
    let mut instance = sekai_app::SekaiInstance::init(&root.store()).await.unwrap();
    let spawn = name("spawn");
    let first = instance
        .create_scope(&spawn, None, vec![(OVER, Rect::new(0, 0, 0, 0))], false)
        .await
        .unwrap();

    let err = instance
        .create_scope(&spawn, None, vec![(OVER, Rect::new(1, 1, 1, 1))], false)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::ScopeExists { name } if name == "spawn"));
    assert_eq!(instance.list_scopes().await.unwrap(), vec![first.clone()]);

    let replaced = instance
        .create_scope(
            &spawn,
            Some(vec![RegionKind::Poi]),
            vec![(OVER, Rect::new(1, 1, 1, 1))],
            true,
        )
        .await
        .unwrap();
    assert_eq!(replaced.created_at_ms, first.created_at_ms);
    assert_eq!(instance.list_scopes().await.unwrap(), vec![replaced]);
}

#[tokio::test]
async fn a_selection_of_nothing_is_refused() {
    let root = TempDir::new("nothing");
    let mut instance = sekai_app::SekaiInstance::init(&root.store()).await.unwrap();
    let err = instance
        .create_scope(&name("spawn"), None, Vec::new(), false)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        AppError::InvalidScope(NamedScopeError::NoAreas)
    ));
    let err = instance
        .create_scope(
            &name("spawn"),
            Some(Vec::new()),
            vec![(OVER, Rect::new(0, 0, 0, 0))],
            false,
        )
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        AppError::InvalidScope(NamedScopeError::NoKinds)
    ));
    assert_eq!(instance.list_scopes().await.unwrap(), Vec::new());
}

#[tokio::test]
async fn a_scope_resolves_to_its_rectangles() {
    let root = TempDir::new("resolve");
    let mut instance = sekai_app::SekaiInstance::init(&root.store()).await.unwrap();
    let err = instance.resolve_scope(&name("spawn")).await.unwrap_err();
    assert!(matches!(err, AppError::UnknownScope { name } if name == "spawn"));

    instance
        .create_scope(
            &name("spawn"),
            Some(vec![RegionKind::Entities, RegionKind::Region]),
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
            kinds: vec![RegionKind::Region, RegionKind::Entities],
            areas: vec![
                (OVER, Area::Rect(Rect::region(0, 0))),
                (Dimension::NETHER, Area::Rect(Rect::new(0, 0, 0, 0))),
            ],
        }
    );
}

#[tokio::test]
async fn a_resolved_scope_limits_a_commit() {
    let root = TempDir::new("commit");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, vec![1]), (1, 0, vec![2]), (5, 5, vec![3])],
    );
    let mut instance = sekai_app::SekaiInstance::init(&root.store()).await.unwrap();
    instance
        .create_scope(
            &name("spawn"),
            None,
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
