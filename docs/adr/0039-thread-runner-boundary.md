# ADR-0039: ThreadRunner is the parallelism boundary; app names no runtime

- Status: Accepted
- Date: 2026-11-23

## Context

Every fan-out in `app` (ingest, cache proof/update, world-side diff
reads, checkout writes) was a hand-rolled bounded loop over
`tokio::task::JoinSet`: spawn `worker_slots(concurrency)` tasks, join in
completion order, respawn from a queued iterator. The shape was
duplicated five times, and it pinned the orchestration library to
tokio: a `no_std`/wasm embedding could not reuse the flows even though
every layer below `app` is runtime-free. Per-operation `concurrency`
fields on the options structs also scattered one instance-wide decision
across every call site.

The file work itself cannot simply move onto async tasks: the
`WorldTree` boundary is synchronous, and the local backend's adapters
(`spawn_blocking` inside sled ops, sqlx's tokio driver) panic when
driven from a thread with no runtime context. So the split has to be
by executor, not by task: synchronous file work on one side, async
store calls on the other.

## Decision

- `sekai-util` owns the one execution boundary: `ThreadRunner`
  (`new(parallelism)`, `run(items, f) -> impl JobSet<R>`,
  `call(f) -> impl Future`) and `JobSet` (`join_next().await`,
  `try_join_next()`, `cancel()`), all `no_std` + `alloc`. A worker
  panic surfaces as `WorkerPanic` in place of that item's output.
- `OsThread`, the host runner, lives in the `sekai` binary
  (`sekai-cli`): one OS thread per slot, workers pulling from a shared
  `Mutex<Iterator>`, completions in a bounded queue (one slot per
  worker) with condvar backpressure and a waker-driven join future.
  `util` stays purely `no_std` (no features); its `Sequential` is the
  inline reference runner.
- Per-item functions are synchronous by contract. Async store calls
  (CAS puts, cache puts, checkout blob fetches) stay on the caller's
  executor, interleaved with the joins; workers keep staging the next
  files meanwhile, so the pipeline stays overlapped and the bytes in
  flight stay bounded by the fan-out.
- `SekaiInstance<S: SekaiStore, T: ThreadRunner = Sequential>` composes
  the runner once; `open`/`init` keep the inline default,
  `open_with`/`init_with`/`new` take one. The CLI builds
  `<OsThread as ThreadRunner>::new(jobs)` from `--jobs`. The
  `concurrency` fields leave the options structs: parallelism belongs
  to the runner, not to each operation.
- Checkout feeds prepared writes through a bounded
  `std::sync::mpsc::sync_channel` used as the runner's iterator: the
  caller fetches blobs, the pool rebuilds and writes. The feed joins
  completions whenever every slot is busy, and the run drains the pool
  to retirement even after a failure, so no worker still holds a world
  lock share when the call returns.
- Lock semantics follow ADR-0022: each job holds its `Arc<WorldGuard>`
  share until it ends. Dropping a job set stops the queued items and
  returns at once (a cancelled run never blocks its caller); in-flight
  jobs finish on their own threads. Workers release the closure's
  captures before signalling retirement, so joining a run to
  exhaustion guarantees every share is free.
- `app` drops its tokio dependency (it stays a dev-dependency for the
  tests); `AppError::Join(JoinError)` is replaced by `WorkerPanic`
  attributed through `StorageError::backend`.

## Consequences

The fan-out shape lives once, in the runner, and the flows read as
"run these items, join completions, do the async part here". A
`no_std`/wasm embedding composes its own `ThreadRunner` (or
`Sequential`) and reuses every flow unchanged; the storage traits stay
async and tokio-backed below. The cost is one extra hand-off per file
in checkout (fetch on the caller, write on the pool) and a sequential
CAS pass per joined file - both were already the effective shape, since
sled puts serialize on `spawn_blocking` anyway. Error paths wait for
in-flight jobs (at most one per slot) before returning, which makes
lock release deterministic for a caller that retries immediately.