//! End-to-end commit pruning over real world folders and SQLite stores.
//!
//! Requires the `backend-sqlite` feature.

#![cfg(feature = "backend-sqlite")]

use sekai_app::{HostWorldTree, SekaiStore};
use std::path::Path;

/// A temp dir whose name starts with the calling test's name (tempfile
/// appends a random suffix), removed when its guard is dropped at the end
/// of the test, panics included.
fn tempdir(name: &str) -> (std::path::PathBuf, tempfile::TempDir) {
    let guard = tempfile::Builder::new()
        .prefix(&format!("sekai-prune-{name}-"))
        .tempdir()
        .unwrap();
    (guard.path().to_path_buf(), guard)
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

fn commit_options() -> sekai_app::CommitOptions {
    sekai_app::CommitOptions {
        concurrency: 2,
        ..sekai_app::CommitOptions::default()
    }
}

/// Stored blob count, read through a second handle on the same store.
async fn blob_count(store: impl AsRef<Path>) -> usize {
    let opened = sekai_storage_sqlite::open_sqlite(store).await.unwrap();
    let mut count = 0usize;
    sekai_core::BlobStore::visit_blobs(opened.blob(), |_| {
        count += 1;
        true
    })
    .await
    .unwrap();
    count
}

#[tokio::test]
async fn prune_folds_and_gc_reclaims() {
    let (root, _guard) = tempdir("lifecycle");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store_dir = root.join("store");
    let store = store_dir.to_string_lossy().into_owned();
    let region = world_path.join("region/r.0.0.mca");

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();

    // Three commits with distinct payloads.
    write_region(&region, &[(0, 0, vec![3, 1])]);
    instance
        .world_mut(&mut world)
        .commit(commit_options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    write_region(&region, &[(0, 0, vec![3, 2])]);
    instance
        .world_mut(&mut world)
        .commit(commit_options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    write_region(&region, &[(0, 0, vec![3, 3])]);
    instance
        .world_mut(&mut world)
        .commit(commit_options(), sekai_app::Scope::World, |_| {})
        .await
        .unwrap();
    let commits = instance.list_commits().await.unwrap();
    assert_eq!(commits.len(), 3);
    assert_eq!(blob_count(&store_dir).await, 3);

    let doomed_name = sekai_app::RefName::parse("doomed").unwrap();
    instance
        .create_ref(&doomed_name, Some(commits[0].id), false)
        .await
        .unwrap();

    // Dry-run first: one deletion planned, nothing touched.
    let (plan, plan_timings) = instance.prune_plan(Some(2), None).await.unwrap();
    assert_eq!(plan.delete, [commits[0].id]);
    assert_eq!(plan.retained, [commits[1].id, commits[2].id]);
    assert_eq!(plan_timings.apply, std::time::Duration::ZERO);
    assert_eq!(instance.list_commits().await.unwrap().len(), 3);

    let (report, _) = instance.prune(Some(2), None, |_| {}).await.unwrap();
    assert_eq!(report.pruned, 1);
    assert_eq!(report.rows_folded + report.rows_dropped, 1);
    let remaining = instance.list_commits().await.unwrap();
    assert_eq!(remaining.len(), 2);
    let doomed_ref = instance
        .list_refs()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.name == doomed_name)
        .expect("doomed ref carried onto the successor");
    assert_eq!(doomed_ref.commit, Some(commits[1].id));
    assert_eq!(
        instance.resolve_commit_ref("doomed").await.unwrap(),
        commits[1].id
    );

    // Retained commits still restore faithfully through fallback.
    let (checked_out, _) = instance
        .world_mut(&mut world)
        .checkout(
            commits[1].id,
            sekai_app::CheckoutOptions::default(),
            sekai_app::Scope::World,
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(checked_out.chunks_restored, 1);

    // Pruning dereferences but never unlinks: GC reclaims exactly one blob.
    assert_eq!(blob_count(&store_dir).await, 3);
    let (gc_report, _) = instance.gc(|_| {}).await.unwrap();
    assert_eq!(gc_report.removed, 1);
    assert_eq!(blob_count(&store_dir).await, 2);
}

#[tokio::test]
async fn prune_before_ref_and_empty_guards() {
    use sekai_app::CommitId;
    let (root, _guard) = tempdir("guards");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store = root.join("store").to_string_lossy().into_owned();
    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();
    for payload in [vec![3, 1], vec![3, 2], vec![3, 3]] {
        write_region(world_path.join("region/r.0.0.mca"), &[(0, 0, payload)]);
        instance
            .world_mut(&mut world)
            .commit(commit_options(), sekai_app::Scope::World, |_| {})
            .await
            .unwrap();
    }

    let commits = instance.list_commits().await.unwrap();
    let second = commits[1].id;
    let third = commits[2].id;

    // `--before` the second commit retains it and newer.
    let (report, _) = instance.prune(None, Some(second), |_| {}).await.unwrap();
    assert_eq!(report.pruned, 1);
    assert_eq!(instance.list_commits().await.unwrap().len(), 2);

    // Intersected with keep-last: only the newest commit survives.
    let (plan, _) = instance.prune_plan(Some(1), Some(second)).await.unwrap();
    assert_eq!(plan.delete, [second]);
    assert_eq!(plan.retained, [third]);

    // Selections retaining nothing fail loudly instead of wiping the store.
    assert!(instance.prune(Some(0), None, |_| {}).await.is_err());
    assert!(
        instance
            .prune(None, Some(CommitId([0xFF; 32])), |_| {})
            .await
            .is_err()
    );
    assert_eq!(instance.list_commits().await.unwrap().len(), 2);
}
