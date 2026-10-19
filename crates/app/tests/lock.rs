//! Cross-process exclusion: every operation locks the store (and its world)
//! for its own duration. A second handle in the same process conflicts
//! exactly like another process would, so held locks stand in for
//! concurrent runs here.

use sekai_app::{
    BackupOptions, BlobHash, ExportOptions, HostWorldTree, RollbackOptions, Scope, SekaiInstance,
    SnapshotId, StatusOptions, TagName, WorldLockMode, WorldTree,
};
use sekai_core::MetaStore as _;
use sekai_storage::{StorageError, StoreLockMode};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use StoreLockMode::{Exclusive, Shared, Writer};

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
    /// A blob snapshot 1 references, for blob-level operations.
    blob: BlobHash,
}

impl Fixture {
    async fn new(name: &str) -> Self {
        let root = tempdir(name);
        let world_path = root.join("world");
        // Uncompressed (3) empty root compounds: valid chunks that decode.
        write_region(world_path.join("region/r.0.0.mca"), &[3, 10, 0, 0, 0]);
        write_region(world_path.join("region/r.1.0.mca"), &[3, 10, 0, 0, 0]);
        let mut world = HostWorldTree::new(&world_path).unwrap();
        let store = root.join("store").to_string_lossy().into_owned();
        let mut instance = SekaiInstance::init(&store).await.unwrap();
        instance
            .world_mut(&mut world)
            .backup(BackupOptions::default(), Scope::World, |_| {})
            .await
            .unwrap();
        let coord = instance.snapshot_chunk_coords(SnapshotId(1)).await.unwrap()[0];
        let blob = instance
            .store()
            .meta()
            .lookup_chunk(SnapshotId(1), &coord)
            .await
            .unwrap()
            .and_then(|entry| entry.blob)
            .unwrap();
        Self {
            root,
            world_path,
            world,
            instance,
            blob,
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

fn is_lock_error(err: &sekai_app::AppError) -> bool {
    match err {
        sekai_app::AppError::Storage(StorageError::StoreLocked { .. }) => true,
        sekai_app::AppError::Io(err) => err.kind() == std::io::ErrorKind::WouldBlock,
        _ => false,
    }
}

/// Every public operation, with the locks ADR-0014 assigns it.
#[derive(Debug, Clone, Copy)]
enum Op {
    ListSnapshots,
    LatestSnapshot,
    ListTags,
    CreateTag,
    DeleteTag,
    ResolveRef,
    SnapshotStats,
    SnapshotCoords,
    DiffBlobs,
    DiffChunks,
    Export,
    GcPlan,
    GcApply,
    Gc,
    PrunePlan,
    PruneApply,
    Prune,
    Backup,
    Status,
    DiffWorld,
    Rollback,
    WorldChunkCoords,
}

impl Op {
    const ALL: [Self; 22] = [
        Self::ListSnapshots,
        Self::LatestSnapshot,
        Self::ListTags,
        Self::CreateTag,
        Self::DeleteTag,
        Self::ResolveRef,
        Self::SnapshotStats,
        Self::SnapshotCoords,
        Self::DiffBlobs,
        Self::DiffChunks,
        Self::Export,
        Self::GcPlan,
        Self::GcApply,
        Self::Gc,
        Self::PrunePlan,
        Self::PruneApply,
        Self::Prune,
        Self::Backup,
        Self::Status,
        Self::DiffWorld,
        Self::Rollback,
        Self::WorldChunkCoords,
    ];

    const fn store_mode(self) -> Option<StoreLockMode> {
        match self {
            Self::GcPlan | Self::GcApply | Self::Gc | Self::Backup => Some(Writer),
            Self::PruneApply | Self::Prune => Some(Exclusive),
            Self::WorldChunkCoords => None,
            _ => Some(Shared),
        }
    }

    const fn world_mode(self) -> Option<WorldLockMode> {
        match self {
            Self::Backup | Self::Status | Self::DiffWorld | Self::WorldChunkCoords => {
                Some(WorldLockMode::Shared)
            }
            Self::Rollback => Some(WorldLockMode::Exclusive),
            _ => None,
        }
    }

    /// Payloads are minimal but valid, so an operation the held lock lets
    /// through must succeed. Prunes keep every snapshot, so the fixture
    /// stays intact across the matrix.
    async fn run(self, fx: &mut Fixture) -> Result<(), sekai_app::AppError> {
        let id = SnapshotId(1);
        let tag = TagName::parse("held").unwrap();
        let export_dir = fx.root.join(format!(
            "export-{}",
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let instance = &mut fx.instance;
        match self {
            Self::ListSnapshots => instance.list_snapshots().await.map(drop),
            Self::LatestSnapshot => instance.latest_snapshot_id().await.map(drop),
            Self::ListTags => instance.list_tags().await.map(drop),
            Self::CreateTag => instance.create_tag(&tag, id, true).await.map(drop),
            Self::DeleteTag => instance.delete_tag(&tag).await.map(drop),
            Self::ResolveRef => instance.resolve_snapshot_ref("1").await.map(drop),
            Self::SnapshotStats => instance.snapshot_stats(id).await.map(drop),
            Self::SnapshotCoords => instance.snapshot_chunk_coords(id).await.map(drop),
            Self::DiffBlobs => instance
                .diff_blobs(&fx.blob, &fx.blob, None)
                .await
                .map(drop),
            Self::DiffChunks => instance
                .diff_chunks(id, id, &[], None, |_| {})
                .await
                .map(drop),
            Self::Export => {
                let out = HostWorldTree::new_legacy(export_dir);
                let outcome = instance
                    .export(&out, id, ExportOptions::default(), Scope::World, |_| {})
                    .await;
                outcome.map(drop)
            }
            Self::GcPlan => instance.gc_plan().await.map(drop),
            Self::GcApply => {
                let plan = sekai_app::GcPlan::new(Vec::new(), 0);
                instance.gc_apply(&plan, |_| {}).await.map(drop)
            }
            Self::Gc => instance.gc(|_| {}).await.map(drop),
            Self::PrunePlan => instance.prune_plan(Some(u64::MAX), None).await.map(drop),
            Self::PruneApply => {
                let plan = sekai_app::PrunePlan {
                    delete: Vec::new(),
                    retained: vec![id],
                };
                instance.prune_apply(&plan, |_| {}).await.map(drop)
            }
            Self::Prune => instance.prune(Some(u64::MAX), None, |_| {}).await.map(drop),
            Self::Backup => instance
                .world_mut(&mut fx.world)
                .backup(BackupOptions::default(), Scope::World, |_| {})
                .await
                .map(drop),
            Self::Status => instance
                .world(&fx.world)
                .status(StatusOptions::default(), Scope::World, |_| {})
                .await
                .map(drop),
            Self::DiffWorld => instance
                .world(&fx.world)
                .diff_world_chunks(None, &[], None, |_| {})
                .await
                .map(drop),
            Self::Rollback => instance
                .world_mut(&mut fx.world)
                .rollback(id, RollbackOptions::default(), Scope::World, |_| {})
                .await
                .map(drop),
            Self::WorldChunkCoords => sekai_app::world_chunk_coords(&fx.world).map(drop),
        }
    }
}

/// A blocked operation fails on the lock; any other must succeed outright,
/// so a content error cannot pass for (or hide) a lock decision.
fn assert_locked_out(blocked: bool, outcome: &Result<(), sekai_app::AppError>, case: &str) {
    if blocked {
        assert!(
            outcome.as_ref().is_err_and(is_lock_error),
            "{case}: {outcome:?}"
        );
    } else {
        assert!(outcome.is_ok(), "{case}: {outcome:?}");
    }
}

const fn store_compatible(held: StoreLockMode, wanted: StoreLockMode) -> bool {
    matches!((held, wanted), (Shared, Shared | Writer) | (Writer, Shared))
}

#[tokio::test]
async fn every_operation_takes_its_store_mode() {
    let mut fx = Fixture::new("store-matrix").await;
    for held in [Shared, Writer, Exclusive] {
        let lock = fx.hold_store(held);
        for op in Op::ALL {
            let blocked = op
                .store_mode()
                .is_some_and(|wanted| !store_compatible(held, wanted));
            let outcome = op.run(&mut fx).await;
            assert_locked_out(
                blocked,
                &outcome,
                &format!("{op:?} with the store held {held:?}"),
            );
        }
        drop(lock);
    }
}

#[tokio::test]
async fn every_operation_takes_its_world_mode() {
    let mut fx = Fixture::new("world-matrix").await;
    let other = fx.other_world();
    for held in [WorldLockMode::Shared, WorldLockMode::Exclusive] {
        let lock = other.lock(held).unwrap();
        for op in Op::ALL {
            let blocked = op.world_mode().is_some_and(|wanted| {
                held == WorldLockMode::Exclusive || wanted == WorldLockMode::Exclusive
            });
            let outcome = op.run(&mut fx).await;
            assert_locked_out(
                blocked,
                &outcome,
                &format!("{op:?} with the world held {held:?}"),
            );
        }
        drop(lock);
    }
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

/// A cancelled rollback future cannot stop its blocking file pass, so the
/// pass itself must keep the world locked until it finishes.
#[tokio::test]
async fn a_cancelled_rollback_keeps_the_world_locked_until_its_files_finish() {
    let mut fx = Fixture::new("cancel").await;
    let other = fx.other_world();
    let (started, files_running) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();

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
            // Parks the file pass until the test lets go (the sender drops).
            let _ = released.recv();
        },
    );
    tokio::select! {
        outcome = rollback => panic!("the rollback must still be running: {outcome:?}"),
        started = tokio::task::spawn_blocking(move || files_running.recv()) => {
            assert!(matches!(started, Ok(Ok(()))), "the file pass never started: {started:?}");
        }
    }
    // The future is gone; its file pass is parked in `progress`.
    assert!(matches!(
        other.lock(WorldLockMode::Exclusive),
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock
    ));

    drop(release);
    let deadline = Instant::now() + Duration::from_secs(10);
    while let Err(err) = other.lock(WorldLockMode::Exclusive) {
        assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock, "{err}");
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
