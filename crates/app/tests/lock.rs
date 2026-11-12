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
    CheckoutOptions, ChunkHash, CommitId, CommitOptions, Dimension, HostRegion, HostWorldLock,
    HostWorldTree, Rect, RefName, RegionFile, RegionFingerprint, RegionKey, RegionKind, Scope,
    ScopeName, SekaiInstance, StatusOptions, WorldError, WorldLockMode, WorldLockState, WorldTree,
};
use sekai_core::MetaStore as _;
use sekai_storage_local::LocalStore;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

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
    const ALL: [Self; 24] = [
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

/// A cancelled checkout future cannot stop the writes already running on the
/// blocking pool, so those writes must keep the world locked until they end.
///
/// Every write waits at a gate, and the future is dropped once one is held
/// there: the world must stay locked while that write waits, then be
/// released once the gate opens, with no write left mid-swap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_checkout_keeps_the_world_locked_until_its_files_finish() {
    let mut fx = Fixture::new("cancel").await;
    let other = fx.other_world();
    let gate = Arc::new(Gate::default());
    let _release = OpenOnDrop(&gate);
    let mut world = GatedTree {
        inner: fx.other_world(),
        gate: Arc::clone(&gate),
    };

    let mut handle = fx.instance.world_mut(&mut world);
    let checkout = handle.checkout(fx.commit, CheckoutOptions::default(), Scope::World, &());
    let mut checkout = Box::pin(checkout);
    poll_until(&mut checkout, || gate.arrived() > 0).await;
    drop(checkout);

    assert!(
        matches!(
            other.lock(WorldLockMode::Exclusive),
            Err(WorldError::WorldLocked { .. })
        ),
        "the world was released while a write was still held"
    );

    gate.open();
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
    assert!(
        !has_swap_in_flight(&fx.world_path),
        "the world was released while a write was still mid-swap"
    );
}

/// The fixture world, except that every region write waits at `gate`.
#[derive(Clone)]
struct GatedTree {
    inner: HostWorldTree,
    gate: Arc<Gate>,
}

impl GatedTree {
    fn gated(&self, inner: HostRegion) -> GatedRegion {
        GatedRegion {
            inner,
            gate: Arc::clone(&self.gate),
        }
    }
}

impl WorldTree for GatedTree {
    type Error = WorldError;
    type Region = GatedRegion;
    type Lock = HostWorldLock;

    fn get_dims(&self) -> Result<Vec<Dimension>, WorldError> {
        self.inner.get_dims()
    }

    fn get_regions(
        &self,
        dim: Dimension,
        kind: RegionKind,
    ) -> Result<Vec<(RegionKey, GatedRegion)>, WorldError> {
        Ok(self
            .inner
            .get_regions(dim, kind)?
            .into_iter()
            .map(|(key, region)| (key, self.gated(region)))
            .collect())
    }

    fn derive_region(&self, key: RegionKey) -> Result<GatedRegion, WorldError> {
        self.inner
            .derive_region(key)
            .map(|region| self.gated(region))
    }

    fn sibling_region(
        &self,
        region: &GatedRegion,
        key: RegionKey,
    ) -> Result<GatedRegion, WorldError> {
        self.inner
            .sibling_region(&region.inner, key)
            .map(|region| self.gated(region))
    }

    fn lock(&self, mode: WorldLockMode) -> Result<WorldLockState<HostWorldLock>, WorldError> {
        self.inner.lock(mode)
    }
}

/// A host region whose writes wait at `gate`.
#[derive(Clone, Debug)]
struct GatedRegion {
    inner: HostRegion,
    gate: Arc<Gate>,
}

impl std::fmt::Display for GatedRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.inner.fmt(f)
    }
}

impl RegionFile for GatedRegion {
    type Error = WorldError;
    type RegionImage = <HostRegion as RegionFile>::RegionImage;

    fn read(&self, key: RegionKey) -> Result<Self::RegionImage, WorldError> {
        self.inner.read(key)
    }

    fn size(&self) -> Result<u64, WorldError> {
        self.inner.size()
    }

    fn fingerprint(&self, key: RegionKey) -> Result<RegionFingerprint, WorldError> {
        self.inner.fingerprint(key)
    }

    fn exists(&self) -> bool {
        self.inner.exists()
    }

    fn write(&self, image: &[u8]) -> Result<(), WorldError> {
        self.gate.pass();
        self.inner.write(image)
    }

    fn remove(&self) -> Result<(), WorldError> {
        self.inner.remove()
    }
}

/// Holds every write that reaches it until opened.
#[derive(Debug, Default)]
struct Gate {
    arrived: AtomicUsize,
    open: Mutex<bool>,
    opened: Condvar,
}

impl Gate {
    /// Count this write in, then wait until the gate is open.
    fn pass(&self) {
        self.arrived.fetch_add(1, Ordering::SeqCst);
        drop(
            self.opened
                .wait_while(self.open.lock().unwrap(), |open| !*open)
                .unwrap(),
        );
    }

    /// Writes that have reached the gate so far.
    fn arrived(&self) -> usize {
        self.arrived.load(Ordering::SeqCst)
    }

    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.opened.notify_all();
    }
}

/// Opens the gate when dropped, so a failing assertion never leaves writes
/// held: the runtime waits for them before the test can end.
struct OpenOnDrop<'a>(&'a Gate);

impl Drop for OpenOnDrop<'_> {
    fn drop(&mut self) {
        self.0.open();
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

/// Poll `fut` until `ready` holds, keeping the future alive.
///
/// The future is driven by hand so the caller decides when to drop it: that
/// drop is the cancellation. Each poll yields, so the work it started gets a
/// chance to run. A regression trips the deadline instead of hanging.
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

/// A crashed run - or a pre-lock binary - leaves `commit.inflight` behind.
/// Nothing reads the marker anymore, so it must not block collection.
#[tokio::test]
async fn a_leftover_run_marker_is_ignored() {
    let mut fx = Fixture::new("marker").await;
    std::fs::write(fx.root.join("store/commit.inflight"), "12345 0\n").unwrap();
    fx.instance.gc(&()).await.unwrap();
}
