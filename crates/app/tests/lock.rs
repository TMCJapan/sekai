//! Cross-process exclusion: every operation that touches a world locks it
//! for its own duration. A second handle in the same process conflicts
//! exactly like another process would, so held locks stand in for
//! concurrent runs here. The store itself never locks: its blobs are
//! append-only and its metadata resolves conflicts at commit time.
//!
//! Requires the `backend-local` feature.

#![cfg(feature = "backend-local")]

mod fixtures;

use fixtures::chunk_nbt;

use sekai_app::{
    CheckoutOptions, ChunkHash, CloneOptions, CommitId, CommitOptions, Dimension, HostWorldTree,
    ProgressBuilder, Rect, RefName, Reporter, Scope, ScopeName, SekaiInstance, StatusOptions,
    WorldError, WorldLockMode, WorldTree,
};
use sekai_core::MetaStore as _;
use sekai_storage_local::LocalStore;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Distinguishes the clone targets one test creates inside a single root.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn write_region(path: impl AsRef<Path>, payload: &[u8]) {
    let name = path.as_ref().file_name().unwrap().to_str().unwrap();
    let (rx, rz) = sekai_anvil::parse_region_name(name).unwrap();
    let mut builder = sekai_anvil::RegionBuilder::new(rx, rz, 0).unwrap();
    builder.stage_chunk(rx * 32, rz * 32, payload).unwrap();
    std::fs::create_dir_all(path.as_ref().parent().unwrap()).unwrap();
    std::fs::write(path, builder.image().unwrap()).unwrap();
}

struct Fixture {
    /// Held so the root outlives the handles that point into it, and is
    /// removed on drop or when `new` unwinds.
    _guard: tempfile::TempDir,
    root: PathBuf,
    world_path: PathBuf,
    world: HostWorldTree,
    instance: SekaiInstance<LocalStore>,
    /// The fixture's only commit.
    commit: CommitId,
    /// A blob that the fixture commit references, for blob-level operations.
    blob: ChunkHash,
    /// A scope no row deletes, so resolving it always succeeds.
    scope: ScopeName,
}

fn lock_matrix_areas() -> Vec<(Dimension, Rect)> {
    vec![(Dimension::OVERWORLD, Rect::region(0, 0))]
}

impl Fixture {
    async fn new(name: &str) -> Self {
        let guard = tempfile::Builder::new()
            .prefix(&format!("sekai-app-lock-{name}-"))
            .tempdir()
            .unwrap();
        let root = guard.path().to_path_buf();
        let world_path = root.join("world");
        let payload = chunk_nbt("minecraft:full");
        write_region(world_path.join("region/r.0.0.mca"), &payload);
        write_region(world_path.join("region/r.1.0.mca"), &payload);
        let mut world = HostWorldTree::new(&world_path).unwrap();
        let store = root.join("store").to_string_lossy().into_owned();
        let mut instance = SekaiInstance::init(&store).await.unwrap();
        let (report, _) = instance
            .world_mut(&mut world)
            .commit(CommitOptions::default(), Scope::World, &())
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
            _guard: guard,
            root,
            world_path,
            world,
            instance,
            commit,
            blob,
            scope,
        }
    }

    /// A second tree over the same folder, standing in for another process.
    fn other_world(&self) -> HostWorldTree {
        HostWorldTree::new(&self.world_path).unwrap()
    }
}

const fn is_lock_error(err: &sekai_app::AppError) -> bool {
    matches!(
        err,
        sekai_app::AppError::World(WorldError::WorldLocked { .. })
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
            Self::DiffBlobs => instance.diff_blobs(&fx.blob, &fx.blob).await.map(drop),
            Self::DiffChunks => instance.diff_chunks(id, id, &[], &()).await.map(drop),
            Self::Clone => {
                let clone_dir = fx.root.join(format!(
                    "clone-{}",
                    TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
                ));
                let out = HostWorldTree::new_legacy(clone_dir);
                let outcome = instance
                    .clone_to(&out, id, CloneOptions::default(), Scope::World, &())
                    .await;
                outcome.map(drop)
            }
            Self::GcPlan => instance.gc_plan().await.map(drop),
            Self::GcApply => {
                let plan = sekai_app::GcPlan::new(Vec::new(), 0);
                instance.gc_apply(&plan, &()).await.map(drop)
            }
            Self::Gc => instance.gc(&()).await.map(drop),
            Self::PrunePlan => instance.prune_plan(Some(u64::MAX), None).await.map(drop),
            Self::PruneApply => {
                let plan = sekai_app::PrunePlan {
                    delete: Vec::new(),
                    retained: vec![id],
                };
                instance.prune_apply(&plan, &()).await.map(drop)
            }
            Self::Prune => instance.prune(Some(u64::MAX), None, &()).await.map(drop),
            Self::Commit => instance
                .world_mut(&mut fx.world)
                .commit(CommitOptions::default(), Scope::World, &())
                .await
                .map(drop),
            Self::Status => instance
                .world(&fx.world)
                .status(StatusOptions::default(), Scope::World, &())
                .await
                .map(drop),
            Self::DiffWorld => instance
                .world(&fx.world)
                .diff_world_chunks(None, &[], &())
                .await
                .map(drop),
            Self::Checkout => instance
                .world_mut(&mut fx.world)
                .checkout(id, CheckoutOptions::default(), Scope::World, &())
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
    fx.instance.gc(&()).await.unwrap();
    fx.instance.prune(Some(1), None, &()).await.unwrap();
    let (plan, _) = fx.instance.gc_plan().await.unwrap();
    fx.instance.gc_apply(&plan, &()).await.unwrap();
}

/// A cancelled checkout future cannot stop its blocking file pass, so the
/// pass itself must keep the world locked until it finishes - and every file
/// it has queued must still land.
///
/// The future is dropped as soon as the pass reports anything, which is where
/// it is still working: `progress` only ticks a counter, because parking it
/// would park the async half this test drives by hand. The world must stay
/// shut until the queued writes are done - an exclusive lock only succeeds
/// once no step is left - and both regions must be on disk by then.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_checkout_keeps_the_world_locked_until_its_files_finish() {
    let mut fx = Fixture::new("cancel").await;
    let other = fx.other_world();
    let reported = Counted::default();

    // A second tree over the same folder: the first one is busy being run.
    let mut world = fx.other_world();
    let mut handle = fx.instance.world_mut(&mut world);
    let checkout = handle.checkout(
        fx.commit,
        CheckoutOptions::default(),
        Scope::World,
        &reported,
    );
    let mut checkout = Box::pin(checkout);
    poll_until(&mut checkout, || reported.count() > 0).await;
    drop(checkout);

    // The future is gone; the pass it queued is not. The world stays shut
    // until every file queued behind the report has been written.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match other.lock(WorldLockMode::Exclusive) {
            Ok(held) => drop(held),
            Err(WorldError::WorldLocked { .. }) => {
                assert!(Instant::now() < deadline, "the file pass never released");
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            Err(err) => panic!("the world lock failed: {err}"),
        }
        break;
    }
    assert_eq!(
        read_regions(&fx.world_path),
        vec!["r.0.0.mca", "r.1.0.mca"],
        "a queued file was abandoned with the future"
    );
}

/// A cancelled clone leaves `out` incomplete, but never with a partial file:
/// each write swaps a finished file into place, and the writes already
/// running finish after the future is gone.
///
/// The future is dropped as soon as a file is reported. Once no write is
/// left mid-swap, every region file in the target must match a full clone
/// of the same commit byte for byte, and every reported file must be there.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_clone_leaves_only_whole_files() {
    let fx = Fixture::new("cancel-clone").await;
    let fresh_target = || {
        fx.root.join(format!(
            "cloned-{}",
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    };
    // Not `HostWorldTree::new`: it creates its folder up front, and clone
    // only accepts an empty target.
    let full = fresh_target();
    fx.instance
        .clone_to(
            &HostWorldTree::new_legacy(&full),
            fx.commit,
            CloneOptions::default(),
            Scope::World,
            &(),
        )
        .await
        .unwrap();

    let target = fresh_target();
    let out = HostWorldTree::new_legacy(&target);
    let reported = Counted::default();
    let clone = fx.instance.clone_to(
        &out,
        fx.commit,
        CloneOptions::default(),
        Scope::World,
        &reported,
    );
    let mut clone = Box::pin(clone);
    poll_until(&mut clone, || reported.count() >= 1).await;
    drop(clone);

    let deadline = Instant::now() + Duration::from_secs(10);
    while has_swap_in_flight(&target) {
        assert!(Instant::now() < deadline, "a write never finished its swap");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let landed = read_regions(&target);
    assert!(
        landed.len() >= reported.count(),
        "a reported file is missing: {landed:?}"
    );
    for name in &landed {
        assert_eq!(
            std::fs::read(target.join("region").join(name)).unwrap(),
            std::fs::read(full.join("region").join(name))
                .unwrap_or_else(|err| panic!("the full clone lacks {name}: {err}")),
            "{name} is not whole"
        );
    }
}

/// Whether a write is still between its temporary file and the swap.
fn has_swap_in_flight(root: impl AsRef<Path>) -> bool {
    std::fs::read_dir(root.as_ref().join("region")).is_ok_and(|entries| {
        entries
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
    })
}

/// Counts the events of a checkout or clone, readable while it runs.
#[derive(Default)]
struct Counted(AtomicUsize);

impl Counted {
    fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

struct Counter<'a>(&'a AtomicUsize);

impl ProgressBuilder for Counted {
    fn start(&self, _: usize) -> impl Reporter {
        Counter(&self.0)
    }
}

impl Reporter for Counter<'_> {
    fn step(&mut self, _: usize) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// Poll `fut` until `ready` holds, keeping the future alive.
///
/// The future is driven by hand so the caller decides when to drop it: that
/// drop is the cancellation. Each poll yields, so the steps it queued get a
/// chance to run. A regression trips the deadline instead of hanging. A pass
/// that finishes before reporting is not cancelled mid-run, so `ready` checks
/// always include the outcome: a truncated world or output still fails.
async fn poll_until<F>(fut: &mut std::pin::Pin<Box<F>>, ready: impl Fn() -> bool)
where
    F: std::future::Future,
{
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        let done = std::future::poll_fn::<_, _>(|cx| {
            std::task::Poll::Ready(fut.as_mut().poll(cx).is_ready())
        })
        .await;
        if done {
            // Past the cancel point either way; the outcome checks below
            // decide whether a truncated result slipped through.
            return;
        }
        assert!(Instant::now() < deadline, "the file pass never reported");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

/// Region file names present under `root/region`, sorted.
///
/// Only the fixture's own world is a folder the test created: a clone target
/// is whatever directory the tree derived, so callers pass that directory.
fn read_regions(root: impl AsRef<Path>) -> Vec<String> {
    let root = root.as_ref();
    let mut names: Vec<String> = std::fs::read_dir(root.join("region"))
        .expect("the region folder exists")
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.to_ascii_lowercase().ends_with(".mca"))
        .collect();
    names.sort();
    names
}

/// A crashed run - or a pre-lock binary - leaves `commit.inflight` behind.
/// Nothing reads the marker anymore, so it must not block collection.
#[tokio::test]
async fn a_leftover_run_marker_is_ignored() {
    let mut fx = Fixture::new("marker").await;
    std::fs::write(fx.root.join("store/commit.inflight"), "12345 0\n").unwrap();
    fx.instance.gc(&()).await.unwrap();
}
