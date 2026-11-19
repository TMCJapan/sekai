//! Checkout execution: rebuild world files from a resolved commit plan.
//!
//! Rationale: the restore set (which blobs each region file needs) comes
//! from the [`core`](sekai_core::usecase::checkout) plan; what stays here
//! is mechanics `core` cannot own: world-layout discovery, path
//! derivation, and the atomic MCA rewrite itself. Region files are rebuilt
//! wholesale from CAS blobs (never patched). Under the default
//! [`CheckoutOptions`] on-disk chunks unknown to the commit (created
//! later) vanish along with post-commit regions, regions absent from the
//! snapshot delete their file instead of leaving a header-only shell, and a
//! blob missing from CAS aborts loudly: that is corruption, and writing a
//! partial world would be worse than writing none. Keep-policies in
//! [`CheckoutOptions`] relax each of those strict behaviors explicitly.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use sekai_core::usecase::checkout::{FileChange, overwritten_regions, unchanged_regions};
use sekai_core::{
    CheckoutPlan, CheckoutReport, ChunkCoord, ChunkHash, CommitId, HeadMove, HeadState, RegionKey,
    ResolveError, Revision, Scope, Target,
};
use sekai_storage::{SekaiStore, StorageError};
use sekai_world::{Region, RegionFile, WorldLock, WorldLockMode, WorldTree};
use tokio::task::JoinSet;

use crate::commit::uncommitted_chunks;
use crate::error::{AppError, BranchUse};
use crate::instance::{WorldGuard, WorldHandleMut};
use crate::progress::{BuildProgress, Progress};
use crate::refs::switch_err;
use crate::regions::discover_regions;
use crate::time::now_ms;
use crate::workers::worker_slots;

/// A restore: the commit it rewrote the area from and what it did.
#[derive(Debug, Clone)]
pub struct Restored {
    /// The commit the area was rewritten from.
    pub commit: CommitId,
    /// What the rebuild wrote and deleted.
    pub report: CheckoutReport,
    /// Per-phase timings.
    pub timings: CheckoutTimings,
}

/// Per-phase timings for checkout execution.
#[derive(Debug, Clone, Default)]
pub struct CheckoutTimings {
    /// Wall-clock total.
    pub total: Duration,
    /// Checkout plan resolution from metadata.
    pub plan: Duration,
    /// Region discovery.
    pub discover: Duration,
    /// Comparing the files to touch with HEAD; zero under `force`.
    pub check: Duration,
    /// Rebuilding MCA files, restoring chunks from CAS, and removing post-commit files.
    pub checkout_files: Duration,
}

/// What to do when a commit blob is absent from CAS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MissingBlobPolicy {
    /// Abort loudly: a missing blob is corruption, and writing a partial
    /// world would be worse than writing none.
    #[default]
    Abort,
    /// Skip the chunk, leaving it out of the rebuilt file.
    SkipChunk,
}

/// Where to rebuild a commit-known region whose file is missing on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MissingFilePolicy {
    /// Prefer a same-dimension sibling's directory, falling back to the
    /// layout-derived location.
    #[default]
    SiblingFirst,
    /// Always use the layout-derived location, ignoring siblings.
    DerivedOnly,
    /// Fail loudly instead of guessing a location.
    Error,
}

/// Checkout restore policy and worker count.
///
/// `Default` is strict: commit-unknown files are deleted, tombstoned
/// chunks vanish, missing blobs abort, and missing files resolve
/// sibling-first. It rebuilds up to one file per CPU at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CheckoutOptions {
    /// Keep region files unknown to the commit instead of deleting them.
    pub keep_post_commit_files: bool,
    /// Keep live bytes for chunks unknown to the commit (created
    /// afterwards inside a commit-known region) instead of dropping them.
    pub keep_post_commit_chunks: bool,
    /// Keep live bytes for commit-tombstoned chunks instead of removing
    /// them. Fully tombstoned region files are left alone rather than
    /// deleted.
    pub keep_tombstoned_chunks: bool,
    /// How to handle a commit blob missing from CAS.
    pub on_missing_blob: MissingBlobPolicy,
    /// Where to rebuild a commit-known region whose file is missing.
    pub on_missing_file: MissingFilePolicy,
    /// How many files rebuild at once. `0` means one per CPU, `1` stays
    /// sequential.
    pub concurrency: usize,
}

/// A branch switch: where HEAD stood and what the world rebuild did.
#[derive(Debug, Clone)]
pub struct Switched {
    /// HEAD before the switch.
    pub from: HeadState,
    /// What the switch selected.
    pub target: Target,
    /// What the rebuild wrote and deleted.
    pub report: CheckoutReport,
    /// Per-phase timings.
    pub timings: CheckoutTimings,
}

impl<W: WorldTree, S: SekaiStore> WorldHandleMut<'_, W, S>
where
    AppError: From<W::Error>,
{
    /// Rebuild `scope` of the bound world from `commit`, returning
    /// execution report and per-phase timings: [`Self::restore`] with
    /// `force`.
    pub async fn checkout(
        &mut self,
        commit: CommitId,
        options: CheckoutOptions,
        scope: Scope,
        progress_builder: &impl BuildProgress,
    ) -> Result<(CheckoutReport, CheckoutTimings), AppError> {
        self.restore(commit, scope, true, options, progress_builder)
            .await
    }

    /// Switch HEAD to `target` and rebuild the world from its commit, as
    /// `checkout [-f] <branch | commit-ish>` does (ADR-0036 *checkout*);
    /// [`crate::SekaiInstance::resolve_target`] resolves the argument.
    ///
    /// A branch attaches HEAD, [`HeadMove::Keep`] keeps HEAD as it is, and
    /// a detached target detaches it. Regions that HEAD's commit and the target's agree
    /// on are left alone, so their uncommitted changes carry over, and a
    /// checkout that would overwrite or drop an uncommitted chunk fails
    /// with [`AppError::WouldOverwrite`] before any file is touched.
    /// `force` skips that check and rebuilds the whole world, as
    /// `git checkout -f` does; without it, `HEAD` changes nothing. A branch
    /// checked out elsewhere is refused before any file is touched, and
    /// HEAD moves only after every file is rebuilt.
    pub async fn switch(
        &mut self,
        target: Target,
        force: bool,
        options: CheckoutOptions,
        progress_builder: &impl BuildProgress,
    ) -> Result<Switched, AppError> {
        let guard = WorldGuard::acquire(self.world, WorldLockMode::Exclusive)?;
        let started = Instant::now();
        let meta = self.instance.store.meta();
        let (worktree, from) =
            sekai_core::head_state(meta, self.instance.worktree.as_ref()).await?;

        // `HEAD` without `force` changes nothing, as `git checkout HEAD` does.
        if target.head == HeadMove::Keep && !force {
            return Ok(Switched {
                from,
                target,
                report: CheckoutReport::default(),
                timings: CheckoutTimings::default(),
            });
        }
        // A branch checked out elsewhere is refused before any file is
        // touched.
        if let HeadMove::Attach(name) = &target.head {
            self.instance
                .refuse_checked_out(name, Some(worktree), BranchUse::CheckOut)
                .await?;
        }

        let commit = target.commit;
        let plan = self.plan_checkout(commit).await?;
        // Regions both commits agree on carry their uncommitted changes over.
        let untouched = match from.commit() {
            Some(head) if !force => {
                let keys: Vec<RegionKey> = plan.groups.keys().copied().collect();
                unchanged_regions(meta, head, commit, &keys).await?
            }
            _ => BTreeSet::new(),
        };
        let rebuild = Rebuild {
            plan,
            planning: started.elapsed(),
            scope: &Scope::World,
            untouched: &untouched,
            force,
        };
        let (report, timings) = self
            .rebuild(rebuild, options, &guard, progress_builder)
            .await?;

        // HEAD moves only after every file is rebuilt.
        sekai_core::switch_head(self.instance.store.meta_mut(), worktree, &target, now_ms()?)
            .await
            .map_err(switch_err)?;
        Ok(Switched {
            from,
            target,
            report,
            timings,
        })
    }

    /// Switch HEAD to what `revision` selects and rebuild the world, as
    /// `checkout [-f] <branch | commit-ish>` does:
    /// [`crate::SekaiInstance::resolve_target`] followed by
    /// [`Self::switch`].
    pub async fn switch_to(
        &mut self,
        revision: &Revision,
        force: bool,
        options: CheckoutOptions,
        progress_builder: &impl BuildProgress,
    ) -> Result<Switched, AppError> {
        let target = self.instance.resolve_target(revision).await?;
        self.switch(target, force, options, progress_builder).await
    }

    /// Rewrite `scope` of the bound world from `source`, or from HEAD's
    /// commit, without moving HEAD, as `checkout [-f] [<commit-ish>]
    /// <selection>` does (ADR-0036 *checkout*). An unborn HEAD fails with
    /// [`AppError::UnbornBranch`].
    pub async fn restore_from(
        &mut self,
        source: Option<&Revision>,
        scope: Scope,
        force: bool,
        options: CheckoutOptions,
        progress_builder: &impl BuildProgress,
    ) -> Result<Restored, AppError> {
        let commit = match source {
            Some(source) => self.instance.resolve_revision(source).await?,
            None => self.born_head().await?,
        };
        let (report, timings) = self
            .restore(commit, scope, force, options, progress_builder)
            .await?;
        Ok(Restored {
            commit,
            report,
            timings,
        })
    }

    /// Act on HEAD alone, as `checkout [-f]` without an argument or a
    /// selection does (ADR-0036 *checkout*): nothing without `force`,
    /// and with it a rebuild of the whole world from HEAD that discards
    /// uncommitted changes. Either way, an unborn HEAD fails with
    /// [`AppError::UnbornBranch`], as in Git.
    pub async fn checkout_head(
        &mut self,
        force: bool,
        options: CheckoutOptions,
        progress_builder: &impl BuildProgress,
    ) -> Result<Option<Restored>, AppError> {
        let commit = self.born_head().await?;
        if !force {
            return Ok(None);
        }
        let (report, timings) = self
            .restore(commit, Scope::World, true, options, progress_builder)
            .await?;
        Ok(Some(Restored {
            commit,
            report,
            timings,
        }))
    }

    /// The commit HEAD stands at; an unborn HEAD fails in Git's words for
    /// `checkout`.
    async fn born_head(&self) -> Result<CommitId, AppError> {
        let instance = &self.instance;
        sekai_core::born_head(instance.store.meta(), instance.worktree.as_ref())
            .await
            .map_err(|err| match err {
                ResolveError::UnbornHead { .. } => AppError::UnbornBranch,
                other => AppError::CommitRef(other),
            })
    }

    /// Rewrite `scope` of the bound world from `commit` without moving
    /// HEAD, as `checkout [-f] [<commit-ish>] <selection>` does, and as
    /// `checkout -f` does for the whole world from HEAD (ADR-0036
    /// *checkout*). Returns execution report and per-phase timings.
    ///
    /// Without `force`, a restore that would overwrite or drop an
    /// uncommitted chunk fails with [`AppError::WouldOverwrite`] before
    /// any file is touched. Region files outside `scope` are never
    /// written, deleted, or otherwise touched. `progress_builder` counts the
    /// files to write or delete, stepping on the async side as each one
    /// finishes, never on the blocking pool.
    ///
    /// What each file needs is decided before any is touched, so refusing a
    /// missing file ([`MissingFilePolicy::Error`]) leaves the world as it
    /// was. A later failure or a cancelled checkout leaves it partly
    /// rebuilt, with every file either rebuilt or untouched, never partial;
    /// the world stays locked until every write already running has ended.
    ///
    /// Rebuilding writes (and may delete) region files, so it goes through
    /// `&mut self`: only one write session per world can exist at a time.
    pub async fn restore(
        &mut self,
        commit: CommitId,
        scope: Scope,
        force: bool,
        options: CheckoutOptions,
        progress_builder: &impl BuildProgress,
    ) -> Result<(CheckoutReport, CheckoutTimings), AppError> {
        let guard = WorldGuard::acquire(self.world, WorldLockMode::Exclusive)?;
        let started = Instant::now();
        let plan = self.plan_checkout(commit).await?;
        let rebuild = Rebuild {
            plan,
            planning: started.elapsed(),
            scope: &scope,
            untouched: &BTreeSet::new(),
            force,
        };
        self.rebuild(rebuild, options, &guard, progress_builder)
            .await
    }

    /// Rebuild the whole bound world from `plan`, a commit already planned,
    /// as [`Self::restore`] does under `force`, under the caller's
    /// exclusive `guard` on the world.
    #[cfg(feature = "backend-local")]
    #[expect(
        clippy::needless_pass_by_ref_mut,
        reason = "rebuilding writes the world, which takes a write session"
    )]
    pub(crate) async fn restore_planned(
        &mut self,
        plan: CheckoutPlan,
        options: CheckoutOptions,
        guard: &Arc<WorldGuard<W::Lock>>,
        progress_builder: &impl BuildProgress,
    ) -> Result<(CheckoutReport, CheckoutTimings), AppError> {
        let rebuild = Rebuild {
            plan,
            planning: Duration::ZERO,
            scope: &Scope::World,
            untouched: &BTreeSet::new(),
            force: true,
        };
        self.rebuild(rebuild, options, guard, progress_builder)
            .await
    }

    /// The present rows of `commit`.
    async fn plan_checkout(&self, commit: CommitId) -> Result<CheckoutPlan, AppError> {
        sekai_core::usecase::checkout::plan_checkout(self.instance.store.meta(), commit)
            .await
            .map_err(AppError::Checkout)
    }

    /// Rebuild the files `rebuild` selects, after checking them for
    /// uncommitted chunks unless `rebuild.force` is set.
    async fn rebuild(
        &self,
        rebuild: Rebuild<'_>,
        options: CheckoutOptions,
        guard: &Arc<WorldGuard<W::Lock>>,
        progress_builder: &impl BuildProgress,
    ) -> Result<(CheckoutReport, CheckoutTimings), AppError> {
        let total_started = Instant::now();
        let world = &*self.world;
        let plan = rebuild.plan;
        // The commit's creation time stamps every rebuilt file.
        let timestamp = u32::try_from(plan.created_at_ms / 1000).unwrap_or(u32::MAX);
        let selected = |key: &RegionKey| {
            rebuild.scope.matches_region(*key) && !rebuild.untouched.contains(key)
        };

        // Select before the file passes: regions left out are dropped from
        // both sides, so the passes below cannot tell they exist.
        let mut groups = plan.groups;
        groups.retain(|key, _| selected(key));

        // Discovery walks the filesystem, so it runs on the blocking pool
        // with its own share of the world lock, like every file write.
        let discover_started = Instant::now();
        let world_handle = world.clone();
        let mut discovered =
            on_blocking_pool(Arc::clone(guard), move || discover_regions(&world_handle)).await?;
        let discover_dt = discover_started.elapsed();
        discovered.retain(|key, _| selected(key));

        let jobs = plan_files(groups, &discovered, world, options)?;

        let check_started = Instant::now();
        if !rebuild.force {
            let uncommitted = uncommitted_chunks(
                self.instance,
                world,
                rebuild.scope,
                options.concurrency,
                guard,
            )
            .await?;
            let keep_live_chunks =
                options.keep_post_commit_chunks || options.keep_tombstoned_chunks;
            let regions = overwritten_regions(
                jobs.iter().map(FileJob::change),
                &uncommitted,
                keep_live_chunks,
            );
            if !regions.is_empty() {
                return Err(AppError::WouldOverwrite { regions });
            }
        }
        let check_dt = check_started.elapsed();

        let files_started = Instant::now();
        let report = run_files(
            jobs,
            self.instance.store.blob(),
            options,
            timestamp,
            guard,
            progress_builder,
        )
        .await?;
        let files_dt = files_started.elapsed();

        let timings = CheckoutTimings {
            total: rebuild.planning + total_started.elapsed(),
            plan: rebuild.planning,
            discover: discover_dt,
            check: check_dt,
            checkout_files: files_dt,
        };
        Ok((report, timings))
    }
}

/// A commit's rows to rebuild, which files they may touch, and whether
/// the files are checked first.
struct Rebuild<'a> {
    /// The commit's present rows.
    plan: CheckoutPlan,
    /// How long resolving and planning took.
    planning: Duration,
    /// Regions outside are never touched.
    scope: &'a Scope,
    /// Regions a branch switch leaves alone.
    untouched: &'a BTreeSet<RegionKey>,
    /// Skip the check for uncommitted chunks.
    force: bool,
}

/// What one region file needs.
enum FileJob<R: RegionFile> {
    Delete {
        key: RegionKey,
        region: R,
    },
    Rebuild {
        key: RegionKey,
        region: R,
        rows: Vec<(ChunkCoord, ChunkHash)>,
    },
}

impl<R: RegionFile> FileJob<R> {
    /// The region and what happens to it, for the uncommitted-chunk check.
    fn change(&self) -> (RegionKey, FileChange<'_>) {
        match self {
            Self::Delete { key, .. } => (*key, FileChange::Delete),
            Self::Rebuild { key, rows, .. } => (*key, FileChange::Rebuild(rows)),
        }
    }
}

/// Decide what every region file needs, without touching the disk: present
/// rows are rebuilt from CAS blobs, absent and post-commit files are
/// deleted unless kept by [`CheckoutOptions`]. Files left alone get no job.
fn plan_files<T: WorldTree>(
    mut groups: BTreeMap<RegionKey, Vec<(ChunkCoord, ChunkHash)>>,
    discovered: &BTreeMap<RegionKey, T::Region>,
    world: &T,
    options: CheckoutOptions,
) -> Result<Vec<FileJob<T::Region>>, AppError>
where
    AppError: From<T::Error>,
{
    let mut keys: BTreeSet<RegionKey> = discovered.keys().copied().collect();
    keys.extend(groups.keys().copied());
    keys.into_iter()
        .filter_map(|key| {
            let rows = groups.remove(&key);
            plan_file(key, rows, discovered, world, options).transpose()
        })
        .collect()
}

/// Decide what `key`'s file needs, or `None` when it is left alone.
fn plan_file<T: WorldTree>(
    key: RegionKey,
    rows: Option<Vec<(ChunkCoord, ChunkHash)>>,
    discovered: &BTreeMap<RegionKey, T::Region>,
    world: &T,
    options: CheckoutOptions,
) -> Result<Option<FileJob<T::Region>>, AppError>
where
    AppError: From<T::Error>,
{
    if let Some(rows) = rows {
        let region = if let Some(region) = discovered.get(&key) {
            region.clone()
        } else {
            missing_region_target(discovered, world, key, options.on_missing_file)?
        };
        Ok(Some(FileJob::Rebuild { key, region, rows }))
    } else {
        // No present rows: the region is absent from the snapshot, whether
        // fully tombstoned at the commit or created afterwards. Absence is
        // the tombstone, so the two cannot be distinguished; either
        // keep-policy preserves the live file, strict checkout deletes it.
        let keep = options.keep_tombstoned_chunks || options.keep_post_commit_files;
        if keep {
            Ok(None)
        } else {
            Ok(discovered
                .get(&key)
                .cloned()
                .map(|region| FileJob::Delete { key, region }))
        }
    }
}

/// Run every job as its own task, at most `options.concurrency` at once
/// (one per CPU by default) since each holds a whole region image. A
/// finished file frees its slot for the next, and `progress` steps as each
/// one is joined.
///
/// Tasks hold the world lock only weakly while they fetch: a share is taken
/// just as a file's write or delete goes to the blocking pool. So when the
/// run fails or is cancelled, the tasks still fetching are aborted without
/// extending the lock, and only the writes already on the blocking pool
/// keep the world locked until they end.
async fn run_files<R, L, C>(
    jobs: Vec<FileJob<R>>,
    cas: &C,
    options: CheckoutOptions,
    timestamp: u32,
    guard: &Arc<WorldGuard<L>>,
    progress_builder: &impl BuildProgress,
) -> Result<CheckoutReport, AppError>
where
    R: RegionFile,
    L: WorldLock,
    C: sekai_storage::BlobStore<Error = StorageError> + Clone + Send + Sync + 'static,
    AppError: From<R::Error>,
{
    let mut progress = progress_builder.build_progress(jobs.len());
    let mut report = CheckoutReport {
        files_written: 0,
        files_deleted: 0,
        chunks_restored: 0,
    };
    let slots = worker_slots(options.concurrency);
    let spawn = |set: &mut JoinSet<Result<Outcome, AppError>>, job: FileJob<R>| {
        set.spawn(run_file(
            job,
            cas.clone(),
            options,
            timestamp,
            Arc::downgrade(guard),
        ));
    };
    let mut queued = jobs.into_iter();
    let mut set = JoinSet::new();
    for job in queued.by_ref().take(slots) {
        spawn(&mut set, job);
    }
    while let Some(joined) = set.join_next().await {
        match joined?? {
            Outcome::Written { chunks } => {
                report.files_written += 1;
                report.chunks_restored += chunks;
            }
            Outcome::Deleted => report.files_deleted += 1,
            // `guard` outlives this loop, so no joined task can find the run
            // gone; only one left running after a return can.
            Outcome::AlreadyAbsent | Outcome::Abandoned => {}
        }
        progress.step();
        if let Some(job) = queued.next() {
            spawn(&mut set, job);
        }
    }
    Ok(report)
}

/// Carry out one file's job: fetch its blobs here, then write (or delete)
/// it on the blocking pool.
async fn run_file<R, L, C>(
    job: FileJob<R>,
    cas: C,
    options: CheckoutOptions,
    timestamp: u32,
    lock: Weak<WorldGuard<L>>,
) -> Result<Outcome, AppError>
where
    R: RegionFile,
    L: WorldLock,
    C: sekai_storage::BlobStore<Error = StorageError>,
    AppError: From<R::Error>,
{
    match job {
        FileJob::Delete { region, .. } => {
            write_while_running(&lock, move || {
                if !region.exists() {
                    return Ok(Outcome::AlreadyAbsent);
                }
                region.remove()?;
                Ok(Outcome::Deleted)
            })
            .await
        }
        FileJob::Rebuild { key, region, rows } => {
            let mut builder = sekai_anvil::RegionBuilder::new(key.rx, key.rz, timestamp)?;
            let mut restored: BTreeSet<(i32, i32)> = BTreeSet::new();
            let mut blob_buf = Vec::new();
            for (coord, hash) in &rows {
                if fetch_or_skip(&cas, hash, options.on_missing_blob, &mut blob_buf).await? {
                    builder.stage_chunk(coord.x, coord.z, &blob_buf)?;
                    restored.insert((coord.x, coord.z));
                }
            }
            let keep_live = options.keep_post_commit_chunks || options.keep_tombstoned_chunks;
            write_while_running(&lock, move || {
                if keep_live {
                    merge_live_chunks(&region, key, &restored, options, &mut builder)?;
                }
                region.write(&builder.image()?)?;
                Ok(Outcome::Written {
                    chunks: restored.len(),
                })
            })
            .await
        }
    }
}

/// Hand `work` to the blocking pool with a fresh share of the world lock,
/// unless the run that spawned this task is already gone.
async fn write_while_running<L: WorldLock>(
    lock: &Weak<WorldGuard<L>>,
    work: impl FnOnce() -> Result<Outcome, AppError> + Send + 'static,
) -> Result<Outcome, AppError> {
    match lock.upgrade() {
        Some(guard) => on_blocking_pool(guard, work).await,
        None => Ok(Outcome::Abandoned),
    }
}

/// Run `work` on the blocking pool with its own share of the world lock:
/// a cancelled future cannot stop it once started, so the world stays
/// locked until it ends.
async fn on_blocking_pool<L: WorldLock, T: Send + 'static>(
    guard: Arc<WorldGuard<L>>,
    work: impl FnOnce() -> Result<T, AppError> + Send + 'static,
) -> Result<T, AppError> {
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        work()
    })
    .await?
}

/// What one file job did to its region file.
enum Outcome {
    /// Rebuilt from `chunks` restored chunks.
    Written { chunks: usize },
    /// Removed.
    Deleted,
    /// Due for deletion, but gone by the time its job ran.
    AlreadyAbsent,
    /// Not started: the run had already failed or been cancelled.
    Abandoned,
}

/// Fetch one blob into `out`, honoring `policy` when CAS has no such blob.
///
/// `false` means the chunk must be skipped; under
/// [`MissingBlobPolicy::Abort`] a missing blob is corruption and propagates.
async fn fetch_or_skip<C: sekai_storage::BlobStore<Error = StorageError>>(
    cas: &C,
    hash: &ChunkHash,
    policy: MissingBlobPolicy,
    out: &mut Vec<u8>,
) -> Result<bool, AppError> {
    match sekai_storage::BlobStore::fetch_into(cas, hash, out).await {
        Ok(()) => Ok(true),
        Err(StorageError::BlobMissing { .. }) if policy == MissingBlobPolicy::SkipChunk => {
            Ok(false)
        }
        Err(source) => Err(source.into()),
    }
}

/// Merge live on-disk chunks that the commit does not restore into
/// `builder`: a chunk absent from a commit-known region is absent from the
/// snapshot, and absence cannot distinguish a tombstone from a post-commit
/// chunk, so either keep-policy preserves it. Missing live files contribute
/// nothing; corrupt ones fail loudly.
fn merge_live_chunks<R: RegionFile>(
    region: &R,
    key: RegionKey,
    restored: &BTreeSet<(i32, i32)>,
    options: CheckoutOptions,
    builder: &mut sekai_anvil::RegionBuilder,
) -> Result<(), AppError>
where
    AppError: From<R::Error>,
{
    if !region.exists() {
        return Ok(());
    }
    let image = region.read(key)?;
    let mut failed = None;
    image.visit_chunks(|chunk| {
        if restored.contains(&(chunk.x, chunk.z)) {
            return true;
        }
        let keep = options.keep_tombstoned_chunks || options.keep_post_commit_chunks;
        if keep && let Err(source) = builder.stage_chunk(chunk.x, chunk.z, chunk.payload) {
            failed = Some(source);
            return false;
        }
        true
    })?;
    if let Some(source) = failed {
        return Err(AppError::Anvil(source));
    }
    Ok(())
}

/// Target region file for a commit-known region whose file is missing on
/// disk, per [`MissingFilePolicy`].
fn missing_region_target<T: WorldTree>(
    discovered: &BTreeMap<RegionKey, T::Region>,
    world: &T,
    key: RegionKey,
    policy: MissingFilePolicy,
) -> Result<T::Region, AppError>
where
    AppError: From<T::Error>,
{
    match policy {
        MissingFilePolicy::Error => Err(AppError::MissingRegionFile { key }),
        MissingFilePolicy::DerivedOnly => Ok(world.derive_region(key)?),
        // Folders may have moved since the commit (e.g. across a 26.1
        // migration), so a same-dimension sibling's directory wins over
        // the layout-derived one. Any sibling of that dimension and family
        // serves - they share the directory. A tree that cannot name the
        // sibling fails loudly: falling back to the derived handle would
        // write the region somewhere else. Without a sibling, a namespace
        // the layout cannot derive fails loudly as well.
        MissingFilePolicy::SiblingFirst => match discovered
            .iter()
            .find(|(found, _)| found.dim == key.dim && found.kind == key.kind)
        {
            Some((_, sibling)) => Ok(world.sibling_region(sibling, key)?),
            None => Ok(world.derive_region(key)?),
        },
    }
}
