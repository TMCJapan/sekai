//! Cross-process exclusion: every operation locks the store (and its world)
//! for its own duration. A second handle in the same process conflicts
//! exactly like another process would, so held locks stand in for
//! concurrent runs here.

use sekai_app::{
    BackupOptions, BlobHash, ChunkCoord, Dimension, ExportOptions, HostWorldTree, RegionKind,
    RollbackOptions, Scope, SekaiInstance, SnapshotId, StatusOptions, TagName, WorldLockMode,
    WorldTree,
};
use sekai_storage::StoreLockMode;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sekai-app-lock-{name}-{}-{}",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_region(path: impl AsRef<Path>, payload: &[u8]) {
    let name = path.as_ref().file_name().unwrap().to_str().unwrap();
    let (rx, rz) = sekai_anvil::parse_region_name(name).unwrap();
    let mut builder = sekai_anvil::RegionBuilder::new(rx, rz, 0).unwrap();
    builder.stage_chunk(rx * 32, rz * 32, payload).unwrap();
    std::fs::create_dir_all(path.as_ref().parent().unwrap()).unwrap();
    std::fs::write(path, builder.image().unwrap()).unwrap();
}

struct Fixture {
    root: PathBuf,
    world_path: PathBuf,
    world: HostWorldTree,
    instance: SekaiInstance,
}

impl Fixture {
    /// A store holding one backup of a two-region world.
    async fn new(name: &str) -> Self {
        let root = tempdir(name);
        let world_path = root.join("world");
        write_region(world_path.join("region/r.0.0.mca"), &[3, 1, 2, 3]);
        write_region(world_path.join("region/r.1.0.mca"), &[3, 4, 5, 6]);
        let mut world = HostWorldTree::new(&world_path).unwrap();
        let store = root.join("store").to_string_lossy().into_owned();
        let mut instance = SekaiInstance::init(&store).await.unwrap();
        instance
            .world_mut(&mut world)
            .backup(BackupOptions::default(), Scope::World, |_| {})
            .await
            .unwrap();
        Self {
            root,
            world_path,
            world,
            instance,
        }
    }

    fn hold_store(&self, mode: StoreLockMode) -> sekai_storage::StoreLock {
        self.instance.store().cas().lock(mode).unwrap()
    }

    /// A second tree over the same folder, standing in for another process.
    fn other_world(&self) -> HostWorldTree {
        HostWorldTree::new(&self.world_path).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A chunk present in the fixture world.
const fn chunk() -> ChunkCoord {
    ChunkCoord::new(Dimension::OVERWORLD, RegionKind::REGION, 0, 0)
}

fn assert_locked<T: std::fmt::Debug>(outcome: Result<T, sekai_app::AppError>) {
    let err = outcome.unwrap_err();
    assert!(err.to_string().contains("in use by another sekai"), "{err}");
}

#[tokio::test]
async fn collection_and_pruning_do_not_trip_over_their_own_phases() {
    let mut fx = Fixture::new("own-phases").await;
    // `gc`/`prune` plan and apply under one lock; re-locking per phase
    // would make them conflict with themselves.
    fx.instance.gc(|_| {}).await.unwrap();
    fx.instance.prune(Some(1), None, |_| {}).await.unwrap();
    let (plan, _) = fx.instance.gc_plan().await.unwrap();
    fx.instance.gc_apply(&plan, |_| {}).await.unwrap();
}

#[tokio::test]
async fn a_writer_excludes_collection_and_backups_but_not_readers() {
    let mut fx = Fixture::new("writer").await;
    let _backup = fx.hold_store(StoreLockMode::Writer);

    assert_locked(fx.instance.gc(|_| {}).await);
    assert_locked(fx.instance.gc_plan().await);
    assert_locked(fx.instance.prune(Some(1), None, |_| {}).await);
    let Fixture {
        instance, world, ..
    } = &mut fx;
    assert_locked(
        instance
            .world_mut(world)
            .backup(BackupOptions::default(), Scope::World, |_| {})
            .await,
    );

    // Readers run alongside a backup.
    let latest = fx.instance.latest_snapshot_id().await.unwrap();
    fx.instance.list_snapshots().await.unwrap();
    fx.instance.snapshot_stats(latest).await.unwrap();
    fx.instance.prune_plan(Some(1), None).await.unwrap();
    fx.instance
        .world(&fx.world)
        .status(StatusOptions::default(), Scope::World, |_| {})
        .await
        .unwrap();
}

#[tokio::test]
async fn pruning_excludes_every_other_operation() {
    let mut fx = Fixture::new("exclusive").await;
    let id = SnapshotId(1);
    let tag = TagName::parse("held").unwrap();
    let (gc_plan, _) = fx.instance.gc_plan().await.unwrap();
    let (prune_plan, _) = fx.instance.prune_plan(Some(1), None).await.unwrap();
    let out = HostWorldTree::new_legacy(fx.root.join("export"));
    let _prune = fx.hold_store(StoreLockMode::Exclusive);

    let instance = &mut fx.instance;
    assert_locked(instance.list_snapshots().await);
    assert_locked(instance.latest_snapshot_id().await);
    assert_locked(instance.list_tags().await);
    assert_locked(instance.create_tag(&tag, id, false).await);
    assert_locked(instance.delete_tag(&tag).await);
    assert_locked(instance.resolve_snapshot_ref("1").await);
    assert_locked(instance.snapshot_stats(id).await);
    assert_locked(instance.snapshot_chunk_coords(id).await);
    assert_locked(
        instance
            .diff_blobs(&BlobHash([0; 32]), &BlobHash([0; 32]), None)
            .await,
    );
    assert_locked(instance.diff_chunk(id, id, &chunk(), None).await);
    assert_locked(instance.diff_chunks(id, id, &[chunk()], None, |_| {}).await);
    assert_locked(
        instance
            .export(&out, id, ExportOptions::default(), Scope::World, |_| {})
            .await,
    );
    assert_locked(instance.gc_plan().await);
    assert_locked(instance.gc_apply(&gc_plan, |_| {}).await);
    assert_locked(instance.gc(|_| {}).await);
    assert_locked(instance.prune_plan(Some(1), None).await);
    assert_locked(instance.prune_apply(&prune_plan, |_| {}).await);
    assert_locked(instance.prune(Some(1), None, |_| {}).await);

    let Fixture {
        instance, world, ..
    } = &mut fx;
    let mut handle = instance.world_mut(world);
    assert_locked(
        handle
            .backup(BackupOptions::default(), Scope::World, |_| {})
            .await,
    );
    assert_locked(
        handle
            .status(StatusOptions::default(), Scope::World, |_| {})
            .await,
    );
    assert_locked(handle.diff_world_chunk(None, &chunk(), None).await);
    assert_locked(
        handle
            .diff_world_chunks(None, &[chunk()], None, |_| {})
            .await,
    );
    assert_locked(
        handle
            .rollback(id, RollbackOptions::default(), Scope::World, |_| {})
            .await,
    );
}

#[tokio::test]
async fn a_rollback_excludes_world_readers_and_readers_exclude_a_rollback() {
    let mut fx = Fixture::new("world").await;
    let other = fx.other_world();

    let reader = other.lock(WorldLockMode::Shared).unwrap();
    let Fixture {
        instance, world, ..
    } = &mut fx;
    assert_locked(
        instance
            .world_mut(world)
            .rollback(
                SnapshotId(1),
                RollbackOptions::default(),
                Scope::World,
                |_| {},
            )
            .await,
    );
    // Readers share the world with each other.
    let reading = fx.instance.world(&fx.world);
    reading
        .status(StatusOptions::default(), Scope::World, |_| {})
        .await
        .unwrap();
    // No coordinates: the lock is under test, not decoding the fixture.
    reading
        .diff_world_chunks(None, &[], None, |_| {})
        .await
        .unwrap();
    sekai_app::world_chunk_coords(&fx.world).unwrap();
    drop(reader);

    let _rollback = other.lock(WorldLockMode::Exclusive).unwrap();
    let reading = fx.instance.world(&fx.world);
    assert_locked(
        reading
            .status(StatusOptions::default(), Scope::World, |_| {})
            .await,
    );
    assert_locked(
        reading
            .diff_world_chunks(None, &[chunk()], None, |_| {})
            .await,
    );
    assert_locked(sekai_app::world_chunk_coords(&fx.world));
    let Fixture {
        instance, world, ..
    } = &mut fx;
    assert_locked(
        instance
            .world_mut(world)
            .backup(BackupOptions::default(), Scope::World, |_| {})
            .await,
    );
}

/// A cancelled rollback future cannot stop its blocking file pass, so the
/// pass itself must keep the world locked until it finishes.
#[tokio::test]
async fn a_cancelled_rollback_keeps_the_world_locked_until_its_files_finish() {
    let mut fx = Fixture::new("cancel").await;
    let other = fx.other_world();
    let (started, files_running) = std::sync::mpsc::channel();

    let Fixture {
        instance, world, ..
    } = &mut fx;
    let mut handle = instance.world_mut(world);
    let rollback = handle.rollback(
        SnapshotId(1),
        RollbackOptions::default(),
        Scope::World,
        move |_| {
            let _ = started.send(());
            std::thread::sleep(Duration::from_millis(300));
        },
    );
    tokio::select! {
        outcome = rollback => panic!("the rollback must still be running: {outcome:?}"),
        _ = tokio::task::spawn_blocking(move || files_running.recv()) => {}
    }
    // The future is gone; its file pass is still sleeping in `progress`.
    assert!(other.lock(WorldLockMode::Exclusive).is_err());

    let deadline = Instant::now() + Duration::from_secs(10);
    while other.lock(WorldLockMode::Exclusive).is_err() {
        assert!(Instant::now() < deadline, "the file pass never released");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Pre-lock binaries left `backup.inflight` behind after a crash; it means
/// nothing now and must not block collection.
#[tokio::test]
async fn a_leftover_run_marker_is_ignored() {
    let mut fx = Fixture::new("marker").await;
    std::fs::write(fx.root.join("store/backup.inflight"), "12345 0\n").unwrap();
    fx.instance.gc(|_| {}).await.unwrap();
}
