//! `worktree add`, `worktree list`, and `worktree remove` over real world
//! folders and local stores (ADR-0035 *`worktree add`*, *Identity and
//! discovery*).
//!
//! Requires the `backend-local` feature.

#![cfg(feature = "backend-local")]

mod fixtures;

use fixtures::chunk_nbt;

use sekai_app::{
    AddedWorktree, AppError, CommitId, Head, HeadState, HostWorldTree, Location, RefName, Revision,
    SEKAI_DIR, Scope, SekaiInstance, WorktreeEntry, WorktreeName, WorldRoot, discover,
};
use sekai_storage::MetaStore as _;
use sekai_storage_local::LocalStore;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// A private temp dir, removed on drop.
fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("sekai-worktree-")
        .tempdir()
        .unwrap()
}

fn write(path: impl AsRef<Path>, bytes: &[u8]) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn write_region(path: impl AsRef<Path>, marker: &str) {
    let mut builder = sekai_anvil::RegionBuilder::new(0, 0, 0).unwrap();
    builder.stage_chunk(0, 0, &chunk_nbt(marker)).unwrap();
    write(path, &builder.image().unwrap());
}

/// Chunk payloads of region `r.0.0.mca` at `path`, keyed by coordinate.
fn chunks(path: impl AsRef<Path>) -> BTreeMap<(i32, i32), Vec<u8>> {
    let image = sekai_anvil::RegionImage::from_bytes(fs::read(path).unwrap(), 0, 0).unwrap();
    let mut map = BTreeMap::new();
    image
        .visit_chunks(|chunk| {
            map.insert((chunk.x, chunk.z), chunk.payload.to_vec());
            true
        })
        .unwrap();
    map
}

fn holding(marker: &str) -> BTreeMap<(i32, i32), Vec<u8>> {
    BTreeMap::from([((0, 0), chunk_nbt(marker))])
}

fn name(raw: &str) -> RefName {
    RefName::parse(raw).unwrap()
}

fn worktree(raw: &str) -> WorktreeName {
    WorktreeName::parse(raw).unwrap()
}

fn rev(raw: &str) -> Revision {
    Revision::parse(raw).unwrap()
}

/// A main worktree in a server directory, one commit on `main`, and a
/// region changed since that commit.
struct Fixture {
    root: tempfile::TempDir,
    server: PathBuf,
    world: PathBuf,
    location: Location,
    instance: SekaiInstance<LocalStore>,
    commit: CommitId,
}

impl Fixture {
    async fn new() -> Self {
        let root = tempdir();
        let server = root.path().canonicalize().unwrap().join("server");
        let world = server.join("world");
        write(server.join("server.properties"), b"level-name=world\n");
        write(server.join("ops.json"), b"[]");
        write(world.join("level.dat"), b"level");
        write(world.join("session.lock"), b"lock");
        write(world.join("data/raids.dat"), b"raids");
        write_region(world.join("region/r.0.0.mca"), "committed");

        let initialized = SekaiInstance::init_world(&world, None::<&Path>)
            .await
            .unwrap();
        let mut instance = initialized.instance;
        let mut tree = HostWorldTree::new(&world).unwrap();
        let commit = instance
            .world_mut(&mut tree)
            .commit(sekai_app::CommitOptions::default(), Scope::World, &())
            .await
            .unwrap()
            .0
            .commit;
        write_region(world.join("region/r.0.0.mca"), "uncommitted");
        let location = discover(&world, None::<&Path>).unwrap();
        Self {
            root,
            server,
            world,
            location,
            instance,
            commit,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().canonicalize().unwrap().join(name)
    }

    async fn add(
        &mut self,
        path: impl AsRef<Path>,
        commit_ish: Option<&str>,
    ) -> Result<AddedWorktree, AppError> {
        let commit_ish = commit_ish.map(rev);
        self.instance
            .add_worktree(&self.location, path, commit_ish.as_ref(), &())
            .await
    }

    async fn list(&self) -> Vec<WorktreeEntry> {
        self.instance
            .list_worktrees(&self.location.store)
            .await
            .unwrap()
    }

    async fn head_log(&self, name: &WorktreeName) -> Vec<String> {
        let mut messages = Vec::new();
        self.instance
            .store()
            .meta()
            .visit_head_log(name, |entry| {
                messages.push(entry.message.clone());
                true
            })
            .await
            .unwrap();
        messages
    }
}

#[tokio::test]
async fn add_copies_the_server_and_rebuilds_regions_from_the_commit() {
    let mut fixture = Fixture::new().await;
    let target = fixture.path("feature");

    let added = fixture.add(&target, None).await.unwrap();
    let root = target.join("world");
    assert_eq!(added.name, worktree("feature"));
    assert_eq!(added.root, root);
    assert_eq!(added.head, Head::Branch(name("feature")));
    assert_eq!(added.commit, fixture.commit);
    assert!(added.created_branch);

    // The server directory is copied around the world, minus the store
    // and the lock files.
    assert_eq!(fs::read(target.join("ops.json")).unwrap(), b"[]");
    assert_eq!(
        fs::read(target.join("server.properties")).unwrap(),
        b"level-name=world\n"
    );
    assert_eq!(fs::read(root.join("level.dat")).unwrap(), b"level");
    assert_eq!(fs::read(root.join("data/raids.dat")).unwrap(), b"raids");
    assert!(!root.join("session.lock").exists());
    // Region files come from the commit, not from the source's changes.
    assert_eq!(chunks(root.join("region/r.0.0.mca")), holding("committed"));
    assert_eq!(
        chunks(fixture.world.join("region/r.0.0.mca")),
        holding("uncommitted")
    );

    // The link file names the store and the worktree, so discovery finds
    // the new world.
    assert_eq!(
        discover(&root, None::<&Path>).unwrap(),
        Location {
            store: fixture.location.store.clone(),
            world: Some(WorldRoot {
                root: root.clone(),
                linked: Some(worktree("feature")),
            }),
        }
    );

    assert_eq!(
        fixture
            .instance
            .store()
            .meta()
            .head(&worktree("feature"))
            .await
            .unwrap(),
        Some(HeadState::Attached {
            branch: name("feature"),
            commit: fixture.commit
        })
    );
    // Git's reflogs: the branch logs its creation, HEAD an empty message.
    let mut branch_log = Vec::new();
    fixture
        .instance
        .store()
        .meta()
        .visit_ref_log(&name("feature"), |entry| {
            branch_log.push(entry.message.clone());
            true
        })
        .await
        .unwrap();
    assert_eq!(branch_log, ["branch: Created from HEAD"]);
    assert_eq!(fixture.head_log(&worktree("feature")).await, [""]);
}

#[tokio::test]
async fn a_commit_ish_attaches_to_a_branch_and_detaches_otherwise() {
    let mut fixture = Fixture::new().await;
    let commit = fixture.commit;
    fixture
        .instance
        .create_tag(&name("v1"), &Revision::head(), false)
        .await
        .unwrap();
    fixture
        .instance
        .create_branch(&name("dev"), &Revision::head(), false)
        .await
        .unwrap();

    let dev = fixture.add(fixture.path("a"), Some("dev")).await.unwrap();
    assert_eq!(
        (dev.head, dev.created_branch),
        (Head::Branch(name("dev")), false)
    );
    let tag = fixture.add(fixture.path("b"), Some("v1")).await.unwrap();
    assert_eq!(tag.head, Head::Detached(commit));
    // `HEAD` detaches too, as `git worktree add <path> HEAD` does.
    let head = fixture.add(fixture.path("c"), Some("HEAD")).await.unwrap();
    assert_eq!(head.head, Head::Detached(commit));

    // A branch checked out elsewhere is refused before the target exists.
    let busy = fixture.path("d");
    assert!(matches!(
        fixture.add(&busy, Some("dev")).await,
        Err(AppError::BranchCheckedOut { .. })
    ));
    assert!(!busy.exists());
}

#[tokio::test]
async fn add_refuses_a_target_in_use_and_a_bad_basename() {
    let mut fixture = Fixture::new().await;

    let full = fixture.path("full");
    write(full.join("keep"), b"keep");
    assert!(matches!(
        fixture.add(&full, None).await,
        Err(AppError::TargetExists { path }) if path == full
    ));
    assert_eq!(fs::read(full.join("keep")).unwrap(), b"keep");

    assert!(matches!(
        fixture.add(fixture.path("a..b"), None).await,
        Err(AppError::InvalidName { kind: sekai_app::NameKind::Branch, name }) if name == "a..b"
    ));
    // The basename names the branch only when no commit-ish does.
    let added = fixture
        .add(fixture.path("a..b"), Some("HEAD"))
        .await
        .unwrap();
    assert_eq!(added.name, worktree("a.b"));
}

#[tokio::test]
async fn an_unborn_head_needs_a_commit_ish_or_an_existing_branch() {
    let mut fixture = Fixture::new().await;
    let main = fixture.instance.worktree().unwrap().clone();
    fixture
        .instance
        .store_mut()
        .meta_mut()
        .set_head(
            &main,
            &Head::Branch(name("unborn")),
            sekai_core::ReflogNote::new("", 0),
        )
        .await
        .unwrap();

    // Where Git would infer `--orphan`, sekai fails.
    let fresh = fixture.path("fresh");
    assert!(matches!(
        fixture.add(&fresh, None).await,
        Err(AppError::CommitRef(
            sekai_core::ResolveError::UnbornHead { .. }
        ))
    ));
    assert!(!fresh.exists());
    // An existing branch needs no HEAD to start from, as in Git.
    let added = fixture.add(fixture.path("main"), None).await.unwrap();
    assert_eq!(added.head, Head::Branch(name("main")));
    assert!(!added.created_branch);
}

#[tokio::test]
async fn a_dimension_the_source_does_not_resolve_fails() {
    let mut fixture = Fixture::new().await;
    let custom = fixture.world.join("dimensions/kingdoms/spawn");
    write_region(custom.join("region/r.0.0.mca"), "spawn");
    let mut tree = HostWorldTree::new(&fixture.world).unwrap();
    fixture
        .instance
        .world_mut(&mut tree)
        .commit(sekai_app::CommitOptions::default(), Scope::World, &())
        .await
        .unwrap();
    fs::remove_dir_all(fixture.world.join("dimensions")).unwrap();

    let target = fixture.path("lost");
    assert!(matches!(
        fixture.add(&target, Some("HEAD")).await,
        Err(AppError::UnresolvedDimension { .. })
    ));
    assert!(!target.exists());
}

#[tokio::test]
async fn a_target_inside_the_copy_root_is_left_out_of_the_copy() {
    let mut fixture = Fixture::new().await;
    let target = fixture.server.join("nested");

    let added = fixture.add(&target, Some("HEAD")).await.unwrap();
    assert_eq!(added.root, target.join("world"));
    assert!(target.join("ops.json").exists());
    assert!(!target.join("nested").exists());
}

#[tokio::test]
async fn a_failure_after_the_target_exists_removes_it() {
    let mut fixture = Fixture::new().await;
    let target = fixture.path("gone");
    fixture.add(&target, Some("HEAD")).await.unwrap();
    // The world goes, and the row stays.
    fs::remove_dir_all(&target).unwrap();
    fs::create_dir(&target).unwrap();

    assert!(matches!(
        fixture.add(&target, Some("HEAD")).await,
        Err(AppError::PathRegistered { worktree: found, .. }) if found == worktree("gone")
    ));
    // Even an empty target that existed before goes, as in Git.
    assert!(!target.exists());
}

#[tokio::test]
async fn list_shows_the_main_worktree_first_and_the_rest_by_path() {
    let mut fixture = Fixture::new().await;
    fixture
        .add(fixture.path("zeta"), Some("HEAD"))
        .await
        .unwrap();
    fixture.add(fixture.path("alpha"), None).await.unwrap();

    let listed: Vec<(String, PathBuf, bool)> = fixture
        .list()
        .await
        .into_iter()
        .map(|entry| (entry.name.to_string(), entry.path, entry.main))
        .collect();
    assert_eq!(
        listed,
        [
            ("world".into(), fixture.world.clone(), true),
            ("alpha".into(), fixture.path("alpha/world"), false),
            ("zeta".into(), fixture.path("zeta/world"), false),
        ]
    );
}

#[tokio::test]
async fn remove_drops_the_row_and_the_link_file_and_keeps_the_world() {
    let mut fixture = Fixture::new().await;
    let store = fixture.location.store.clone();
    fixture
        .add(fixture.path("one"), Some("HEAD"))
        .await
        .unwrap();
    fixture
        .add(fixture.path("two"), Some("HEAD"))
        .await
        .unwrap();
    let one = fixture.path("one/world");

    let removed = fixture
        .instance
        .remove_worktree(&store, "one/world")
        .await
        .unwrap();
    assert_eq!(removed.name, worktree("one"));
    assert!(!one.join(SEKAI_DIR).exists());
    assert!(one.join("region/r.0.0.mca").exists());
    assert_eq!(
        fixture.head_log(&worktree("one")).await,
        Vec::<String>::new()
    );

    // A world already gone is cleared too, by its path.
    let two = fixture.path("two/world");
    fs::remove_dir_all(fixture.path("two")).unwrap();
    let removed = fixture
        .instance
        .remove_worktree(&store, two.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(removed.name, worktree("two"));
    assert_eq!(fixture.list().await.len(), 1);

    assert!(matches!(
        fixture.instance.remove_worktree(&store, "server/world").await,
        Err(AppError::UnremovableWorktree { reason: sekai_app::Unremovable::Main, worktree }) if worktree == "server/world"
    ));
    assert!(matches!(
        fixture.instance.remove_worktree(&store, "nowhere").await,
        Err(AppError::UnremovableWorktree {
            reason: sekai_app::Unremovable::NoMatch,
            ..
        })
    ));
    assert!(fixture.server.join("world").join(SEKAI_DIR).is_dir());
}

#[cfg(unix)]
#[tokio::test]
async fn a_region_directory_behind_a_symlink_out_of_the_copy_fails() {
    let mut fixture = Fixture::new().await;
    // The nether's regions live elsewhere, behind a symlink the copy
    // keeps, and hold a region the commit never saw.
    let ssd = fixture.path("ssd");
    write_region(ssd.join("DIM-1/region/r.0.0.mca"), "nether");
    std::os::unix::fs::symlink(ssd.join("DIM-1"), fixture.world.join("DIM-1")).unwrap();

    let target = fixture.path("linked");
    assert!(matches!(
        fixture.add(&target, Some("HEAD")).await,
        Err(AppError::RegionDirOutsideTarget { path }) if path == target.join("world/DIM-1/region")
    ));
    assert_eq!(
        chunks(ssd.join("DIM-1/region/r.0.0.mca")),
        holding("nether")
    );
    assert!(!target.exists());

    // A link that stays inside the copy is no obstacle.
    fs::remove_file(fixture.world.join("DIM-1")).unwrap();
    fs::rename(&ssd, fixture.server.join("ssd")).unwrap();
    std::os::unix::fs::symlink("../ssd/DIM-1", fixture.world.join("DIM-1")).unwrap();
    let added = fixture.add(&target, Some("HEAD")).await.unwrap();
    assert_eq!(added.report.files_deleted, 1);
    assert!(!target.join("ssd/DIM-1/region/r.0.0.mca").exists());
    assert!(fixture.server.join("ssd/DIM-1/region/r.0.0.mca").exists());
}

#[tokio::test]
async fn a_nested_worktree_is_copied_without_its_link_file() {
    let mut fixture = Fixture::new().await;
    fixture
        .add(fixture.server.join("nested"), Some("HEAD"))
        .await
        .unwrap();

    let other = fixture.path("other");
    fixture.add(&other, Some("HEAD")).await.unwrap();
    assert!(!other.join("nested/world").join(SEKAI_DIR).exists());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn special_files_do_not_stall_the_copy() {
    let mut fixture = Fixture::new().await;
    let fifo = fixture.server.join("console.fifo");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );

    let target = fixture.path("fifo");
    let added = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        fixture.add(&target, Some("HEAD")),
    )
    .await;
    if added.is_err() {
        // Unblock the stalled reader so the runtime can shut down.
        drop(fs::OpenOptions::new().write(true).open(&fifo));
    }
    added.expect("the copy stalled on a FIFO").unwrap();
    assert!(!target.join("console.fifo").exists());
}

/// `path` spelled relative to the current directory, through `..`.
#[cfg(unix)]
fn relative(path: impl AsRef<Path>) -> PathBuf {
    let up = std::env::current_dir().unwrap().components().count() - 1;
    let mut relative: PathBuf = std::iter::repeat_n("..", up).collect();
    relative.push(path.as_ref().strip_prefix("/").unwrap());
    relative
}

#[cfg(unix)]
#[tokio::test]
async fn errors_name_the_target_as_given() {
    let mut fixture = Fixture::new().await;

    let full = relative(fixture.path("full"));
    write(full.join("keep"), b"keep");
    let err = fixture.add(&full, None).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("'{}' already exists", full.display())
    );

    let gone = relative(fixture.path("gone"));
    fixture.add(&gone, Some("HEAD")).await.unwrap();
    fs::remove_dir_all(&gone).unwrap();
    let err = fixture.add(&gone, Some("HEAD")).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "'{}' is a missing but already registered worktree; use 'worktree remove' to clear it",
            gone.display()
        )
    );
}
