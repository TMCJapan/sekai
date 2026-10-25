//! End-to-end commit pruning over real world folders and SQLite stores.
//!
//! Requires the `backend-sqlite` feature.

#![cfg(feature = "backend-sqlite")]

use sekai_app::HostWorldTree;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sekai-prune-{name}-{}-{}",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn cleanup(dir: impl AsRef<Path>) {
    let _ = std::fs::remove_dir_all(dir);
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

fn blob_count(store: impl AsRef<Path>) -> usize {
    let mut count = 0usize;
    let mut dirs = vec![store.as_ref().join("blobs")];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                count += 1;
            }
        }
    }
    count
}

#[tokio::test]
async fn prune_folds_and_gc_reclaims() {
    let root = tempdir("lifecycle");
    let world_path = root.join("world");
    let mut world = HostWorldTree::new(&world_path).unwrap();
    let store_dir = root.join("store");
    let store = store_dir.to_string_lossy().into_owned();
    let region = world_path.join("region/r.0.0.mca");

    let mut instance = sekai_app::SekaiInstance::init(&store).await.unwrap();

    // Three commits with distinct payloads; the oldest is tagged.
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
    assert_eq!(blob_count(&store_dir), 3);

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
    assert_eq!(blob_count(&store_dir), 3);
    let (gc_report, _) = instance.gc(|_| {}).await.unwrap();
    assert_eq!(gc_report.removed, 1);
    assert_eq!(blob_count(&store_dir), 2);
    cleanup(&root);
}

#[tokio::test]
async fn prune_before_ref_and_empty_guards() {
    use sekai_app::CommitId;
    let root = tempdir("guards");
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
    cleanup(&root);
}
