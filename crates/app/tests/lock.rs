//! Cross-process exclusion: every operation locks the store (and its world)
//! for its own duration. A second handle in the same process conflicts
//! exactly like another process would, so held locks stand in for
//! concurrent runs here.
//!
//! Requires the `backend-sqlite` feature.

#![cfg(feature = "backend-sqlite")]

use sekai_app::{
    BlobHash, CheckoutOptions, CloneOptions, CommitId, CommitOptions, Dimension, HostWorldTree,
    Rect, RefName, Scope, ScopeName, SekaiInstance, StatusOptions, WorldError, WorldLockMode,
    WorldTree,
};
use sekai_core::MetaStore as _;
use sekai_storage::{StorageError, StoreLockMode};
use sekai_storage_sqlite::SqliteStore;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
    instance: SekaiInstance<SqliteStore>,
    /// The fixture's only commit.
    commit: CommitId,
    /// A blob that the fixture commit references, for blob-level operations.
    blob: BlobHash,
    /// A scope no row deletes, so resolving it always succeeds.
    scope: ScopeName,
}

fn lock_matrix_areas() -> Vec<(Dimension, Rect)> {
    vec![(Dimension::OVERWORLD, Rect::region(0, 0))]
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
        let (report, _) = instance
            .world_mut(&mut world)
            .commit(CommitOptions::default(), Scope::World, |_| {})
            .await
            .unwrap();
        let commit = report.commit;
        let coords = instance.commit_chunk_coords(commit).await.unwrap();
        let coord = coords.first().expect("the fixture commit holds chunks");
        let blob = instance
            .store()
            .meta()
            .lookup_chunk(commit, coord)
            .await
            .unwrap()
            .and_then(|entry| entry.blob)
            .expect("a present chunk references a blob");
        let scope = ScopeName::parse("fixture").unwrap();
        instance
            .create_scope(&scope, lock_matrix_areas(), false)
            .await
            .unwrap();
        Self {
            root,
            world_path,
            world,
            instance,
            commit,
            blob,
            scope,
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

const fn is_lock_error(err: &sekai_app::AppError) -> bool {
    matches!(
        err,
        sekai_app::AppError::Storage(StorageError::StoreLocked { .. })
            | sekai_app::AppError::World(WorldError::WorldLocked { .. })
    )
}

/// Every public operation, with the locks ADR-0014 assigns it.
#[derive(Debug, Clone, Copy)]
enum Op {
    ListCommits,
    LatestCommit,
    ResolveRef,
    CreateRef,
    DeleteRef,
    CreateScope,
    DeleteScope,
    ListScopes,
    ResolveScope,
    CommitStats,
    CommitCoords,
    DiffBlobs,
    DiffChunks,
    Clone,
    GcPlan,
    GcApply,
    Gc,
    PrunePlan,
    PruneApply,
    Prune,
    Commit,
    Status,
    DiffWorld,
    Checkout,
    WorldChunkCoords,
}

impl Op {
    const ALL: [Self; 25] = [
        Self::ListCommits,
        Self::LatestCommit,
        Self::ResolveRef,
        Self::CreateRef,
        Self::DeleteRef,
        Self::CreateScope,
        Self::DeleteScope,
        Self::ListScopes,
        Self::ResolveScope,
        Self::CommitStats,
        Self::CommitCoords,
        Self::DiffBlobs,
        Self::DiffChunks,
        Self::Clone,
        Self::GcPlan,
        Self::GcApply,
        Self::Gc,
        Self::PrunePlan,
        Self::PruneApply,
        Self::Prune,
        Self::Commit,
        Self::Status,
        Self::DiffWorld,
        Self::Checkout,
        Self::WorldChunkCoords,
    ];

    const fn store_mode(self) -> Option<StoreLockMode> {
        match self {
            Self::GcPlan
            | Self::GcApply
            | Self::Gc
            | Self::Commit
            | Self::CreateRef
            | Self::DeleteRef
            | Self::CreateScope
            | Self::DeleteScope => Some(Writer),
            Self::PruneApply | Self::Prune => Some(Exclusive),
            Self::WorldChunkCoords => None,
            Self::ListCommits
            | Self::LatestCommit
            | Self::ResolveRef
            | Self::ListScopes
            | Self::ResolveScope
            | Self::CommitStats
            | Self::CommitCoords
            | Self::DiffBlobs
            | Self::DiffChunks
            | Self::Clone
            | Self::PrunePlan
            | Self::Status
            | Self::DiffWorld
            | Self::Checkout => Some(Shared),
        }
    }

    const fn world_mode(self) -> Option<WorldLockMode> {
        match self {
            Self::Commit | Self::Status | Self::DiffWorld | Self::WorldChunkCoords => {
                Some(WorldLockMode::Shared)
            }
            Self::Checkout => Some(WorldLockMode::Exclusive),
            Self::ListCommits
            | Self::LatestCommit
            | Self::ResolveRef
            | Self::CreateRef
            | Self::DeleteRef
            | Self::CreateScope
            | Self::DeleteScope
            | Self::ListScopes
            | Self::ResolveScope
            | Self::CommitStats
            | Self::CommitCoords
            | Self::DiffBlobs
            | Self::DiffChunks
            | Self::Clone
            | Self::GcPlan
            | Self::GcApply
            | Self::Gc
            | Self::PrunePlan
            | Self::PruneApply
            | Self::Prune => None,
        }
    }

    /// Payloads are minimal but valid, so an operation the held lock lets
    /// through must succeed. State only grows across the matrix: prunes
    /// keep every commit, commits add newer ones, and checkouts restore
    /// the fixture commit, which every row keeps addressing.
    async fn run(self, fx: &mut Fixture) -> Result<(), sekai_app::AppError> {
        let id = fx.commit;
        let instance = &mut fx.instance;
        match self {
            Self::ListCommits => instance.list_commits().await.map(drop),
            Self::LatestCommit => instance.latest_commit_id().await.map(drop),
            Self::ResolveRef => {
                let raw = fx.commit.to_string();
                instance.resolve_commit_ref(&raw).await.map(drop)
            }
            Self::CreateRef => {
                let name = RefName::parse("lock-matrix").unwrap();
                instance.create_ref(&name, Some(id), true).await.map(drop)
            }
            Self::DeleteRef => {
                let name = RefName::parse("lock-matrix").unwrap();
                instance.delete_ref(&name).await.map(drop)
            }
            Self::CreateScope => {
                let name = ScopeName::parse("lock-matrix").unwrap();
                instance
                    .create_scope(&name, lock_matrix_areas(), true)
                    .await
                    .map(drop)
            }
            Self::DeleteScope => {
                let name = ScopeName::parse("lock-matrix").unwrap();
                instance.delete_scope(&name).await.map(drop)
            }
            Self::ListScopes => instance.list_scopes().await.map(drop),
            Self::ResolveScope => instance.resolve_scope(&fx.scope).await.map(drop),
            Self::CommitStats => instance.commit_stats(id).await.map(drop),
            Self::CommitCoords => instance.commit_chunk_coords(id).await.map(drop),
            Self::DiffBlobs => instance
                .diff_blobs(&fx.blob, &fx.blob, None)
                .await
                .map(drop),
            Self::DiffChunks => instance
                .diff_chunks(id, id, &[], None, |_| {})
                .await
                .map(drop),
            Self::Clone => {
                let clone_dir = fx.root.join(format!(
                    "clone-{}",
                    TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
                ));
                let out = HostWorldTree::new_legacy(clone_dir);
                let outcome = instance
                    .clone_to(&out, id, CloneOptions::default(), Scope::World, |_| {})
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
            Self::Commit => instance
                .world_mut(&mut fx.world)
                .commit(CommitOptions::default(), Scope::World, |_| {})
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
            Self::Checkout => instance
                .world_mut(&mut fx.world)
                .checkout(id, CheckoutOptions::default(), Scope::World, |_| {})
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
async fn every_operation_succeeds_with_nothing_held() {
    // The baseline for both matrices: a failure there is a lock decision,
    // never a fixture that cannot run the operation at all.
    let mut fx = Fixture::new("baseline").await;
    for op in Op::ALL {
        let outcome = op.run(&mut fx).await;
        assert_locked_out(false, &outcome, &format!("{op:?} with nothing held"));
    }
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
    // `gc`/`prune` plan and apply under the lock they already hold;
    // taking a phase's lock again inside the run (say, `gc` going through
    // `gc_plan`) would conflict with that hold.
    fx.instance.gc(|_| {}).await.unwrap();
    fx.instance.prune(Some(1), None, |_| {}).await.unwrap();
    let (plan, _) = fx.instance.gc_plan().await.unwrap();
    fx.instance.gc_apply(&plan, |_| {}).await.unwrap();
}

/// A cancelled checkout future cannot stop its blocking file pass, so the
/// pass itself must keep the world locked until it finishes.
#[tokio::test]
async fn a_cancelled_checkout_keeps_the_world_locked_until_its_files_finish() {
    let mut fx = Fixture::new("cancel").await;
    let other = fx.other_world();
    let (started, files_running) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();

    let Fixture {
        instance,
        world,
        commit,
        ..
    } = &mut fx;
    let mut handle = instance.world_mut(world);
    let checkout = handle.checkout(
        *commit,
        CheckoutOptions::default(),
        Scope::World,
        move |_| {
            let _ = started.send(());
            // Parks the file pass until the test lets go (the sender drops).
            let _ = released.recv();
        },
    );
    tokio::select! {
        outcome = checkout => panic!("the checkout must still be running: {outcome:?}"),
        started = tokio::task::spawn_blocking(move || files_running.recv()) => {
            assert!(matches!(started, Ok(Ok(()))), "the file pass never started: {started:?}");
        }
    }
    // The future is gone; its file pass is parked in `progress`.
    assert!(matches!(
        other.lock(WorldLockMode::Exclusive),
        Err(WorldError::WorldLocked { .. })
    ));

    drop(release);
    let deadline = Instant::now() + Duration::from_secs(10);
    while let Err(err) = other.lock(WorldLockMode::Exclusive) {
        assert!(
            matches!(err, WorldError::WorldLocked { .. }),
            "the file pass still holds the world: {err}"
        );
        assert!(Instant::now() < deadline, "the file pass never released");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A cancelled clone future cannot stop its blocking file pass, so the
/// pass itself must keep the store locked until it finishes: `prune`
/// (exclusive) may not delete rows the pass is still reading.
#[tokio::test]
async fn a_cancelled_clone_keeps_the_store_locked_until_its_files_finish() {
    let fx = Fixture::new("cancel-clone").await;
    let (started, files_running) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let files_done = Arc::new(AtomicUsize::new(0));
    let files_total = Arc::new(AtomicUsize::new(0));

    let out = HostWorldTree::new_legacy(fx.root.join(format!(
        "cloned-{}",
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    )));
    let clone = fx
        .instance
        .clone_to(&out, fx.commit, CloneOptions::default(), Scope::World, {
            let done = Arc::clone(&files_done);
            let total = Arc::clone(&files_total);
            move |update| {
                done.store(update.files_done, Ordering::Relaxed);
                total.store(update.files_total, Ordering::Relaxed);
                let _ = started.send(());
                // Parks the file pass until the test lets go (the sender drops).
                let _ = released.recv();
            }
        });
    tokio::select! {
        outcome = clone => panic!("the clone must still be running: {outcome:?}"),
        started = tokio::task::spawn_blocking(move || files_running.recv()) => {
            assert!(matches!(started, Ok(Ok(()))), "the file pass never started: {started:?}");
        }
    }
    // The future is gone; its file pass is parked in `progress`, still
    // holding the store lock.
    let probe = fx.instance.store().cas().lock(Exclusive);
    assert!(
        matches!(&probe, Err(StorageError::StoreLocked { .. })),
        "the store must stay locked while the clone file pass runs: {probe:?}"
    );

    drop(release);
    // The pass reports the files it writes; the last one means the file
    // work is done, and the lock goes with it.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (done, total) = (
            files_done.load(Ordering::Relaxed),
            files_total.load(Ordering::Relaxed),
        );
        if total > 0 && done >= total {
            break;
        }
        assert!(Instant::now() < deadline, "the file pass never finished");
        std::thread::sleep(Duration::from_millis(20));
    }
    loop {
        match fx.instance.store().cas().lock(Exclusive) {
            Ok(lock) => {
                drop(lock);
                break;
            }
            Err(err) => {
                assert!(
                    matches!(err, StorageError::StoreLocked { .. }),
                    "the file pass still holds the store: {err}"
                );
                assert!(Instant::now() < deadline, "the file pass never released");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// A crashed run - or a pre-lock binary - leaves `commit.inflight` behind.
/// The store locks carry the exclusion now, so the marker means nothing and
/// must not block collection.
#[tokio::test]
async fn a_leftover_run_marker_is_ignored() {
    let mut fx = Fixture::new("marker").await;
    std::fs::write(fx.root.join("store/commit.inflight"), "12345 0\n").unwrap();
    fx.instance.gc(|_| {}).await.unwrap();
}
