//! End-to-end `checkout` switching and restoring over local stores
//! (ADR-0036 *checkout*).
//!
//! Requires the `backend-local` feature.

#![cfg(feature = "backend-local")]

mod fixtures;
mod support;

use fixtures::chunk_nbt;

use sekai_app::{
    AppError, Area, CheckoutOptions, CheckoutReport, CommitId, Dimension, HeadState, HostWorldTree,
    Rect, RefName, RegionKey, RegionKind, Revision, Scope, SekaiInstance, WorktreeName,
};
use sekai_core::{Head, ReflogNote, Worktree};
use sekai_storage::MetaStore as _;
use sekai_storage_local::LocalStore;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A private temp dir, removed on drop.
fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("sekai-switch-")
        .tempdir()
        .unwrap()
}

fn write_region(path: impl AsRef<Path>, chunks: &[(i32, i32, &str)]) {
    let name = path.as_ref().file_name().unwrap().to_str().unwrap();
    let (rx, rz) = sekai_anvil::parse_region_name(name).unwrap();
    let mut builder = sekai_anvil::RegionBuilder::new(rx, rz, 0).unwrap();
    for (x, z, marker) in chunks {
        builder.stage_chunk(*x, *z, &chunk_nbt(marker)).unwrap();
    }
    std::fs::create_dir_all(path.as_ref().parent().unwrap()).unwrap();
    std::fs::write(path, builder.image().unwrap()).unwrap();
}

/// Chunk payloads keyed by coordinate; checkout rewrites the layout, so
/// raw bytes differ.
fn chunks(path: impl AsRef<Path>) -> BTreeMap<(i32, i32), Vec<u8>> {
    let name = path.as_ref().file_name().unwrap().to_str().unwrap();
    let (rx, rz) = sekai_anvil::parse_region_name(name).unwrap();
    let image = sekai_anvil::RegionImage::from_bytes(std::fs::read(path).unwrap(), rx, rz).unwrap();
    let mut map = BTreeMap::new();
    image
        .visit_chunks(|chunk| {
            map.insert((chunk.x, chunk.z), chunk.payload.to_vec());
            true
        })
        .unwrap();
    map
}

fn holding(markers: &[(i32, i32, &str)]) -> BTreeMap<(i32, i32), Vec<u8>> {
    markers
        .iter()
        .map(|(x, z, marker)| ((*x, *z), chunk_nbt(marker)))
        .collect()
}

fn name(raw: &str) -> RefName {
    RefName::parse(raw).unwrap()
}

fn rev(raw: &str) -> Revision {
    Revision::parse(raw).unwrap()
}

const fn region_key(rx: i32) -> RegionKey {
    RegionKey::new(Dimension::OVERWORLD, RegionKind::Region, rx, 0)
}

/// A world on `main` and branch `side`, one commit each, that agree on
/// region `a` and disagree on region `b`.
struct Fixture {
    _root: tempfile::TempDir,
    world: HostWorldTree,
    instance: SekaiInstance<LocalStore>,
    a: PathBuf,
    b: PathBuf,
    on_main: CommitId,
    on_side: CommitId,
}

impl Fixture {
    async fn new() -> Self {
        let root = tempdir();
        let world_path = root.path().join("world");
        let (a, b) = (
            world_path.join("region/r.0.0.mca"),
            world_path.join("region/r.1.0.mca"),
        );
        write_region(&a, &[(0, 0, "a1")]);
        write_region(&b, &[(32, 0, "b1")]);
        let world = HostWorldTree::new(&world_path).unwrap();
        let store = root.path().join("store").to_string_lossy().into_owned();
        let instance = support::init_store(&store).await.unwrap();
        let mut fixture = Self {
            _root: root,
            world,
            instance,
            a,
            b,
            on_main: CommitId([0; 32]),
            on_side: CommitId([0; 32]),
        };
        fixture.on_main = fixture.commit().await;
        fixture
            .instance
            .create_branch(&name("side"), &Revision::head(), false)
            .await
            .unwrap();
        fixture.switch("side", false).await.unwrap();
        write_region(&fixture.b, &[(32, 0, "b2")]);
        fixture.on_side = fixture.commit().await;
        fixture.switch("main", false).await.unwrap();
        fixture
    }

    async fn commit(&mut self) -> CommitId {
        self.instance
            .world_mut(&mut self.world)
            .commit(sekai_app::CommitOptions::default(), Scope::World, &())
            .await
            .unwrap()
            .0
            .commit
    }

    async fn switch(&mut self, target: &str, force: bool) -> Result<sekai_app::Switched, AppError> {
        self.switch_with(target, force, CheckoutOptions::default())
            .await
    }

    async fn switch_with(
        &mut self,
        target: &str,
        force: bool,
        options: CheckoutOptions,
    ) -> Result<sekai_app::Switched, AppError> {
        let target = self.instance.resolve_target(&rev(target)).await?;
        self.instance
            .world_mut(&mut self.world)
            .switch(target, force, options, &())
            .await
    }

    async fn restore(
        &mut self,
        source: &str,
        scope: Scope,
        force: bool,
    ) -> Result<CheckoutReport, AppError> {
        let commit = self.instance.resolve_revision(&rev(source)).await?;
        self.instance
            .world_mut(&mut self.world)
            .restore(commit, scope, force, CheckoutOptions::default(), &())
            .await
            .map(|(report, _)| report)
    }

    async fn head(&self) -> HeadState {
        self.instance.head().await.unwrap().unwrap()
    }
}

fn refused(result: Result<sekai_app::Switched, AppError>) -> Vec<RegionKey> {
    match result {
        Err(AppError::WouldOverwrite { regions }) => regions,
        other => panic!("expected WouldOverwrite, got {other:?}"),
    }
}

#[tokio::test]
async fn a_switch_rebuilds_only_regions_the_commits_disagree_on() {
    let mut fx = Fixture::new().await;
    assert_eq!(chunks(&fx.b), holding(&[(32, 0, "b1")]));

    // A change to a region both commits agree on carries over, as in Git.
    write_region(&fx.a, &[(0, 0, "a-local")]);
    let switched = fx.switch("side", false).await.unwrap();
    assert_eq!(switched.report.files_written, 1);
    assert_eq!(chunks(&fx.a), holding(&[(0, 0, "a-local")]));
    assert_eq!(chunks(&fx.b), holding(&[(32, 0, "b2")]));
    assert_eq!(
        fx.head().await,
        HeadState::Attached {
            branch: name("side"),
            commit: fx.on_side
        }
    );
    let log = fx
        .instance
        .resolve_reflog(&sekai_app::ReflogSelector::Head)
        .await
        .unwrap();
    assert_eq!(
        fx.instance.read_reflog(&log, 1).await.unwrap()[0].message,
        "checkout: moving from main to side"
    );
}

#[tokio::test]
async fn a_switch_refuses_to_overwrite_uncommitted_chunks() {
    let mut fx = Fixture::new().await;

    // `b` differs between the commits, so its change would be lost.
    write_region(&fx.b, &[(32, 0, "b-local")]);
    assert_eq!(refused(fx.switch("side", false).await), [region_key(1)]);
    assert_eq!(chunks(&fx.b), holding(&[(32, 0, "b-local")]));
    assert_eq!(fx.head().await.branch(), Some(&name("main")));

    // `force` discards every change, in agreed regions too.
    write_region(&fx.a, &[(0, 0, "a-local")]);
    fx.switch("side", true).await.unwrap();
    assert_eq!(chunks(&fx.a), holding(&[(0, 0, "a1")]));
    assert_eq!(chunks(&fx.b), holding(&[(32, 0, "b2")]));
}

#[tokio::test]
async fn keep_options_let_uncommitted_data_through() {
    let mut fx = Fixture::new().await;

    // A chunk neither commit has is dropped unless kept.
    write_region(&fx.b, &[(32, 0, "b1"), (33, 0, "new")]);
    assert_eq!(refused(fx.switch("side", false).await), [region_key(1)]);
    let keep_chunks = CheckoutOptions {
        keep_post_commit_chunks: true,
        ..CheckoutOptions::default()
    };
    fx.switch_with("side", false, keep_chunks).await.unwrap();
    assert_eq!(chunks(&fx.b), holding(&[(32, 0, "b2"), (33, 0, "new")]));

    // A region file the target lacks is deleted unless kept.
    let c = fx.a.with_file_name("r.2.0.mca");
    write_region(&c, &[(64, 0, "c")]);
    write_region(&fx.b, &[(32, 0, "b2")]);
    assert_eq!(refused(fx.switch("main", false).await), [region_key(2)]);
    let keep_files = CheckoutOptions {
        keep_post_commit_files: true,
        ..CheckoutOptions::default()
    };
    fx.switch_with("main", false, keep_files).await.unwrap();
    assert_eq!(chunks(&c), holding(&[(64, 0, "c")]));
}

#[tokio::test]
async fn a_chunk_gone_from_disk_loses_nothing() {
    let mut fx = Fixture::new().await;
    std::fs::remove_file(&fx.b).unwrap();
    fx.switch("side", false).await.unwrap();
    assert_eq!(chunks(&fx.b), holding(&[(32, 0, "b2")]));
}

#[tokio::test]
async fn other_commit_ishes_detach_and_head_stays() {
    let mut fx = Fixture::new().await;
    let on_side = fx.on_side.to_string();

    fx.switch(&on_side, false).await.unwrap();
    assert_eq!(fx.head().await, HeadState::Detached { commit: fx.on_side });

    // `HEAD` keeps HEAD and touches nothing without `force`.
    write_region(&fx.b, &[(32, 0, "b-local")]);
    let kept = fx.switch("HEAD", false).await.unwrap();
    assert_eq!(kept.report.files_written, 0);
    assert_eq!(chunks(&fx.b), holding(&[(32, 0, "b-local")]));
    fx.switch("HEAD", true).await.unwrap();
    assert_eq!(chunks(&fx.b), holding(&[(32, 0, "b2")]));
    assert_eq!(fx.head().await, HeadState::Detached { commit: fx.on_side });
}

#[tokio::test]
async fn a_branch_checked_out_elsewhere_is_refused_first() {
    let mut fx = Fixture::new().await;
    let linked = Worktree {
        name: WorktreeName::parse("linked").unwrap(),
        path: Some("/worlds/linked".into()),
        head: Head::Branch(name("side")),
    };
    fx.instance
        .store_mut()
        .meta_mut()
        .add_worktree(&linked, ReflogNote::new("", 0))
        .await
        .unwrap();

    let err = fx.switch("side", true).await.unwrap_err();
    assert!(
        matches!(&err, AppError::BranchCheckedOut { branch, .. } if branch == "side"),
        "{err}"
    );
    assert_eq!(chunks(&fx.b), holding(&[(32, 0, "b1")]));
}

#[tokio::test]
async fn an_unborn_head_has_every_chunk_uncommitted() {
    let mut fx = Fixture::new().await;
    let main = WorktreeName::parse("main").unwrap();
    fx.instance
        .store_mut()
        .meta_mut()
        .set_head(&main, &Head::Branch(name("orphan")), ReflogNote::new("", 0))
        .await
        .unwrap();

    assert_eq!(
        refused(fx.switch("side", false).await),
        [region_key(0), region_key(1)]
    );
    fx.switch("side", true).await.unwrap();
    assert_eq!(fx.head().await.commit(), Some(fx.on_side));
}

#[tokio::test]
async fn a_restore_rewrites_the_area_and_keeps_head() {
    let mut fx = Fixture::new().await;
    let only_b = Scope::Select {
        kinds: Scope::all_kinds(),
        areas: vec![(Dimension::OVERWORLD, Area::Rect(Rect::region(1, 0)))],
    };

    let restored = fx.restore("side", only_b.clone(), false).await.unwrap();
    assert_eq!(restored.files_written, 1);
    assert_eq!(chunks(&fx.b), holding(&[(32, 0, "b2")]));
    assert_eq!(fx.head().await.commit(), Some(fx.on_main));

    // The check covers the area too, unlike Git's pathspec checkout.
    write_region(&fx.b, &[(32, 0, "b-local")]);
    assert!(matches!(
        fx.restore("HEAD", only_b, false).await,
        Err(AppError::WouldOverwrite { regions }) if regions == [region_key(1)]
    ));

    // `checkout -f` rebuilds the whole world from HEAD.
    fx.restore("HEAD", Scope::World, true).await.unwrap();
    assert_eq!(chunks(&fx.b), holding(&[(32, 0, "b1")]));
}
