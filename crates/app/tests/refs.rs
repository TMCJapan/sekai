//! End-to-end ref flows over local stores.
//!
//! Requires the `backend-local` feature.

#![cfg(feature = "backend-local")]

mod fixtures;
mod support;

use fixtures::chunk_nbt;

use sekai_app::{
    AppError, BranchEntry, CommitRef, DEFAULT_REF_NAME, HeadState, HostWorldTree, Lookup, NameKind,
    Ref, RefName, RefUpdate, ReflogSelector, Renamed, Revision, WorktreeName,
};
use sekai_core::{Head, ReflogNote};
use sekai_storage::MetaStore as _;
use std::path::Path;

/// A private temp dir, removed on drop. Nothing is kept, so a panicking
/// test leaves nothing behind either.
fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("sekai-ref-")
        .tempdir()
        .unwrap()
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

async fn commit<S: sekai_app::SekaiStore>(
    instance: &mut sekai_app::SekaiInstance<S>,
    world_tree: &mut HostWorldTree,
) -> sekai_app::CommitReport {
    instance
        .world_mut(world_tree)
        .commit(
            sekai_app::CommitOptions::default(),
            sekai_app::Scope::World,
            &(),
        )
        .await
        .unwrap()
        .0
}

fn main_branch() -> RefName {
    name(DEFAULT_REF_NAME)
}

fn main_worktree() -> WorktreeName {
    WorktreeName::parse("main").unwrap()
}

/// Point the main worktree's HEAD at `head` behind the app's back, to
/// reach states no app call makes yet.
async fn set_main_head<S: sekai_app::SekaiStore>(
    instance: &mut sekai_app::SekaiInstance<S>,
    head: Head,
) {
    instance
        .store_mut()
        .meta_mut()
        .set_head(&main_worktree(), &head, ReflogNote::new("", 0))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_fresh_store_starts_unborn_on_main() {
    let root = tempdir();
    let store = root.path().join("store").to_string_lossy().into_owned();
    let instance = support::init_store(&store).await.unwrap();

    // Unborn: HEAD names `main`, which has no commit and no row yet.
    assert_eq!(instance.list_refs().await.unwrap(), []);
    assert_eq!(
        instance.head().await.unwrap(),
        Some(HeadState::Unborn {
            branch: main_branch()
        })
    );
    assert_eq!(instance.current_commit_id().await.unwrap(), None);

    // Initializing again changes nothing. sled locks the store, so the
    // first handle goes before the second opens.
    let head = instance.head().await.unwrap();
    drop(instance);
    let again = support::init_store(&store).await.unwrap();
    assert_eq!(again.head().await.unwrap(), head);
}

#[tokio::test]
async fn a_commit_moves_heads_branch() {
    let root = tempdir();
    let world_path = root.path().join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.path().join("store").to_string_lossy().into_owned();
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, chunk_nbt("marker-1"))],
    );

    let mut instance = support::init_store(&store).await.unwrap();
    let first = commit(&mut instance, &mut world).await;

    // The first commit created HEAD's branch.
    assert_eq!(
        instance.head().await.unwrap(),
        Some(HeadState::Attached {
            branch: main_branch(),
            commit: first.commit
        })
    );
    assert_eq!(
        instance.list_refs().await.unwrap(),
        [CommitRef::new(Ref::branch(main_branch()), first.commit)]
    );

    // A second commit continues it.
    let second = commit(&mut instance, &mut world).await;
    assert_eq!(
        instance.commit_parents(second.commit).await.unwrap(),
        vec![first.commit]
    );
    assert_eq!(
        instance.current_commit_id().await.unwrap(),
        Some(second.commit)
    );
}

#[tokio::test]
async fn switching_refs_forks_the_history() {
    let root = tempdir();
    let world_path = root.path().join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.path().join("store").to_string_lossy().into_owned();
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, chunk_nbt("marker-1"))],
    );

    let mut instance = support::init_store(&store).await.unwrap();
    let first = commit(&mut instance, &mut world).await;

    let feature = RefName::parse("feature").unwrap();
    let start = rev(&first.commit.to_string());
    let record = instance
        .create_branch(&feature, &start, false)
        .await
        .unwrap();
    assert_eq!(record.commit, first.commit);

    // Moving an existing ref needs `force`.
    assert!(matches!(
        instance.create_branch(&feature, &start, false).await,
        Err(sekai_app::AppError::AlreadyExists { kind: sekai_app::NameKind::Branch, name }) if name == feature.as_str()
    ));

    // The branch HEAD is on decides where the next commit lands.
    set_main_head(&mut instance, Head::Branch(feature.clone())).await;
    assert_eq!(instance.head_branch().await.unwrap(), Some(feature.clone()));
    let on_feature = commit(&mut instance, &mut world).await;
    assert_eq!(
        instance.commit_parents(on_feature.commit).await.unwrap(),
        vec![first.commit]
    );

    // Back on `main`, the next commit forks instead of continuing.
    set_main_head(&mut instance, Head::Branch(main_branch())).await;
    let on_main = commit(&mut instance, &mut world).await;
    assert_eq!(
        instance.commit_parents(on_main.commit).await.unwrap(),
        vec![first.commit]
    );
    assert_ne!(on_main.commit, on_feature.commit);

    // Ref names resolve like commit references.
    assert_eq!(
        instance
            .resolve_revision(&Revision::parse("feature").unwrap())
            .await
            .unwrap(),
        on_feature.commit
    );
}

#[tokio::test]
async fn the_checked_out_branch_is_protected() {
    let root = tempdir();
    let world_path = root.path().join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.path().join("store").to_string_lossy().into_owned();
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, chunk_nbt("marker-1"))],
    );
    let mut instance = support::init_store(&store).await.unwrap();
    let first = commit(&mut instance, &mut world).await;
    let second = commit(&mut instance, &mut world).await;

    // The branch HEAD is on is neither deleted nor moved by force.
    for err in [
        instance
            .delete_branch(&main_branch(), false)
            .await
            .unwrap_err(),
        instance
            .create_branch(&main_branch(), &rev(&first.commit.to_string()), true)
            .await
            .unwrap_err(),
    ] {
        assert!(
            matches!(&err, sekai_app::AppError::BranchCheckedOut { branch, .. } if branch == "main"),
            "{err}"
        );
    }
    assert_eq!(
        instance.current_commit_id().await.unwrap(),
        Some(second.commit)
    );

    let other = RefName::parse("other").unwrap();
    instance
        .create_branch(&other, &rev(&first.commit.to_string()), false)
        .await
        .unwrap();
    instance
        .create_branch(&other, &rev(&second.commit.to_string()), true)
        .await
        .unwrap();
    instance.delete_branch(&other, false).await.unwrap();
    assert!(matches!(
        instance.delete_branch(&other, false).await,
        Err(sekai_app::AppError::NotFound { .. })
    ));
    assert_eq!(instance.list_refs().await.unwrap().len(), 1);
}

#[tokio::test]
async fn ref_writes_report_a_missing_target() {
    let root = tempdir();
    let store = root.path().join("store").to_string_lossy().into_owned();
    let mut instance = support::init_store(&store).await.unwrap();
    let feature = RefName::parse("feature").unwrap();

    // A target no commit carries is refused before the ref exists.
    let err = instance
        .create_branch(&feature, &rev(&"63".repeat(32)), false)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            sekai_app::AppError::CommitRef(sekai_core::ResolveError::UnknownCommit {
                id: sekai_app::CommitId(id)
            }) if id == [99; 32]
        ),
        "{err}"
    );
    assert_eq!(instance.list_refs().await.unwrap(), []);
    assert_eq!(instance.head_branch().await.unwrap(), Some(main_branch()));
}

fn name(raw: &str) -> RefName {
    RefName::parse(raw).unwrap()
}

fn rev(raw: &str) -> Revision {
    Revision::parse(raw).unwrap()
}

/// A world with one region file and a store initialized for it.
async fn world_and_store(
    root: &tempfile::TempDir,
) -> (
    HostWorldTree,
    sekai_app::SekaiInstance<sekai_storage_local::LocalStore>,
) {
    let world_path = root.path().join("world");
    write_region(
        world_path.join("region/r.0.0.mca"),
        &[(0, 0, chunk_nbt("marker-1"))],
    );
    let world = HostWorldTree::new(&world_path).unwrap();
    let store = root.path().join("store").to_string_lossy().into_owned();
    (world, support::init_store(&store).await.unwrap())
}

async fn reflog_messages<S: sekai_app::SekaiStore>(
    instance: &sekai_app::SekaiInstance<S>,
    selector: &ReflogSelector,
) -> Vec<String> {
    let log = instance.resolve_reflog(selector).await.unwrap();
    let entries = instance.read_reflog(&log, usize::MAX).await.unwrap();
    entries.into_iter().map(|entry| entry.message).collect()
}

#[tokio::test]
async fn branches_log_their_start_as_written() {
    let root = tempdir();
    let (mut world, mut instance) = world_and_store(&root).await;
    let first = commit(&mut instance, &mut world).await;
    let second = commit(&mut instance, &mut world).await;
    let x = name("x");

    let created = instance
        .create_branch(&x, &rev("HEAD~1"), false)
        .await
        .unwrap();
    assert_eq!(
        created,
        RefUpdate {
            commit: first.commit,
            previous: None,
        }
    );
    // As in Git, the name is refused before the start resolves.
    assert!(matches!(
        instance.create_branch(&x, &rev("nosuch"), false).await,
        Err(AppError::AlreadyExists { kind: NameKind::Branch, name }) if name == "x"
    ));
    assert!(matches!(
        instance.create_branch(&main_branch(), &rev("nosuch"), true).await,
        Err(AppError::BranchCheckedOut { branch, .. }) if branch == "main"
    ));
    let moved = instance
        .create_branch(&x, &Revision::head(), true)
        .await
        .unwrap();
    assert_eq!(moved.commit, second.commit);
    assert_eq!(moved.previous, Some(first.commit));

    // As in Git: `git branch x HEAD~1` then `git branch -f x` (git 2.53).
    assert_eq!(
        reflog_messages(&instance, &ReflogSelector::Ref(x.clone())).await,
        ["branch: Reset to HEAD", "branch: Created from HEAD~1"]
    );
    let log = instance
        .resolve_reflog(&ReflogSelector::Ref(name("heads/x")))
        .await
        .unwrap();
    assert_eq!(log.label(), "heads/x");
    assert_eq!(instance.read_reflog(&log, 1).await.unwrap().len(), 1);
}

#[tokio::test]
async fn deleting_a_branch_asks_whether_head_reaches_it() {
    let root = tempdir();
    let (mut world, mut instance) = world_and_store(&root).await;
    let first = commit(&mut instance, &mut world).await;
    let side = name("side");
    instance
        .create_branch(&side, &Revision::head(), false)
        .await
        .unwrap();
    set_main_head(&mut instance, Head::Branch(side.clone())).await;
    let on_side = commit(&mut instance, &mut world).await;
    set_main_head(&mut instance, Head::Branch(main_branch())).await;

    let err = instance.delete_branch(&side, false).await.unwrap_err();
    assert!(
        matches!(&err, AppError::NotFullyMerged { branch } if branch == "side"),
        "{err}"
    );
    assert_eq!(
        instance.delete_branch(&side, true).await.unwrap(),
        on_side.commit
    );

    // A branch HEAD reaches goes without `force`.
    let old = name("old");
    instance
        .create_branch(&old, &Revision::head(), false)
        .await
        .unwrap();
    assert_eq!(
        instance.delete_branch(&old, false).await.unwrap(),
        first.commit
    );

    // HEAD's own branch is refused before the merge question.
    for force in [false, true] {
        let err = instance
            .delete_branch(&main_branch(), force)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, AppError::BranchCheckedOut { branch, .. } if branch == "main"),
            "{err}"
        );
    }
    assert!(matches!(
        instance.delete_branch(&old, true).await,
        Err(AppError::NotFound { kind: Lookup::Branch, name }) if name == "old"
    ));
}

#[tokio::test]
async fn an_unborn_head_reaches_no_branch() {
    let root = tempdir();
    let (mut world, mut instance) = world_and_store(&root).await;
    commit(&mut instance, &mut world).await;
    let x = name("x");
    instance
        .create_branch(&x, &Revision::head(), false)
        .await
        .unwrap();
    set_main_head(&mut instance, Head::Branch(name("unborn"))).await;

    // As in Git: `git branch -d` on an orphan branch (git 2.53).
    assert!(matches!(
        instance.delete_branch(&x, false).await,
        Err(AppError::NotFullyMerged { .. })
    ));
    // The branch an unborn HEAD names is checked out too.
    assert!(matches!(
        instance.delete_branch(&name("unborn"), true).await,
        Err(AppError::BranchCheckedOut { branch, .. }) if branch == "unborn"
    ));
}

#[tokio::test]
async fn renaming_follows_heads_branch() {
    let root = tempdir();
    let (mut world, mut instance) = world_and_store(&root).await;

    // One name renames HEAD's branch, unborn too.
    let trunk = name("trunk");
    assert_eq!(
        instance.rename_branch(None, &trunk, false).await.unwrap(),
        Renamed {
            old: main_branch(),
            commit: None
        }
    );
    assert_eq!(
        instance.head().await.unwrap(),
        Some(HeadState::Unborn {
            branch: trunk.clone()
        })
    );

    let first = commit(&mut instance, &mut world).await;
    let x = name("x");
    instance
        .create_branch(&x, &Revision::head(), false)
        .await
        .unwrap();
    assert!(matches!(
        instance.rename_branch(None, &x, false).await,
        Err(AppError::AlreadyExists { kind: NameKind::Branch, name }) if name == "x"
    ));
    instance.rename_branch(None, &x, true).await.unwrap();
    assert_eq!(
        instance.list_branches(&[]).await.unwrap(),
        [BranchEntry {
            branch: CommitRef::new(Ref::branch(x.clone()), first.commit),
            worktree: Some(main_worktree()),
            head: true,
        }]
    );
    assert_eq!(
        reflog_messages(&instance, &ReflogSelector::Head).await[0],
        "Branch: renamed refs/heads/trunk to refs/heads/x"
    );

    // A detached HEAD has no branch to rename.
    set_main_head(&mut instance, Head::Detached(first.commit)).await;
    assert!(matches!(
        instance.rename_branch(None, &name("y"), false).await,
        Err(AppError::RenameDetachedHead)
    ));
    instance
        .rename_branch(Some(&x), &name("y"), false)
        .await
        .unwrap();
}

#[tokio::test]
async fn tags_report_what_they_replace() {
    let root = tempdir();
    let (mut world, mut instance) = world_and_store(&root).await;
    let first = commit(&mut instance, &mut world).await;
    let second = commit(&mut instance, &mut world).await;
    let v1 = name("v1");

    let created = instance
        .create_tag(&v1, &rev("HEAD~1"), false)
        .await
        .unwrap();
    assert_eq!(created.commit, first.commit);
    assert!(matches!(
        instance.create_tag(&v1, &Revision::head(), false).await,
        Err(AppError::AlreadyExists {
            kind: NameKind::Tag,
            ..
        })
    ));
    // Unlike a branch, a tag's target resolves first, as in Git.
    assert!(matches!(
        instance.create_tag(&v1, &rev("nosuch"), false).await,
        Err(AppError::CommitRef(
            sekai_core::ResolveError::UnknownRef { .. }
        ))
    ));
    let replaced = instance
        .create_tag(&v1, &Revision::head(), true)
        .await
        .unwrap();
    assert_eq!(replaced.previous, Some(first.commit));

    // Tags keep no reflog (ADR-0035).
    assert!(matches!(
        instance
            .resolve_reflog(&ReflogSelector::Ref(v1.clone()))
            .await,
        Err(AppError::CommitRef(
            sekai_core::ResolveError::NoReflog { .. }
        ))
    ));
    assert_eq!(instance.delete_tag(&v1).await.unwrap(), second.commit);
    assert!(matches!(
        instance.delete_tag(&v1).await,
        Err(AppError::NotFound { kind: Lookup::Tag, name }) if name == "v1"
    ));
}

#[tokio::test]
async fn outside_every_worktree_head_is_unreadable() {
    let root = tempdir();
    let (mut world, mut instance) = world_and_store(&root).await;
    let first = commit(&mut instance, &mut world).await;
    let mut instance = instance.with_worktree(None);
    let x = name("x");

    assert_eq!(instance.head().await.unwrap(), None);
    let err = instance
        .world_mut(&mut world)
        .commit(
            sekai_app::CommitOptions::default(),
            sekai_app::Scope::World,
            &(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            AppError::CommitRef(sekai_core::ResolveError::NoWorktree)
        ),
        "{err}"
    );

    assert!(matches!(
        instance.create_branch(&x, &Revision::head(), false).await,
        Err(AppError::CommitRef(sekai_core::ResolveError::NoWorktree))
    ));
    instance
        .create_branch(&x, &rev(&first.commit.to_string()), false)
        .await
        .unwrap();
    assert!(matches!(
        instance.delete_branch(&x, false).await,
        Err(AppError::CommitRef(sekai_core::ResolveError::NoWorktree))
    ));
    assert!(matches!(
        instance.rename_branch(None, &name("y"), false).await,
        Err(AppError::CommitRef(sekai_core::ResolveError::NoWorktree))
    ));
    assert_eq!(
        instance.delete_branch(&x, true).await.unwrap(),
        first.commit
    );
}
