# ADR-0014: OS file locks exclude concurrent operations

- Status: Accepted
- Date: 2026-10-18

## Context

ADR-0011 kept `gc` out of a backup's write window with a marker file. It
only covered that one pair, and a crashed backup left the marker behind,
blocking `gc` until an operator deleted it. Other pairs were unguarded:

- Two backups race on snapshot IDs.
- `prune` deletes rows a reader resolved moments earlier. A rollback can
  then restore a partial snapshot, or fail halfway through its file pass
  once a following `gc` unlinks the blobs.
- A rollback rewrites region files that a concurrent backup, status, or
  diff is reading, and two rollbacks would interleave their swaps.

## Decision

Exclusion uses `std::fs::File::try_lock`/`try_lock_shared` (`flock` on
Unix, `LockFileEx` on Windows). The OS releases these locks when the
holder dies, so a crash can never wedge anything. Contention fails
immediately instead of waiting.

The store root holds two lock files, created on first use and never
deleted:

| Mode | `writer.lock` | `store.lock` | Operations |
|---|---|---|---|
| shared | - | shared | list, status, diff, export, rollback, tags, prune plan |
| writer | exclusive | shared | backup, gc plan/apply, creating a missing store |
| exclusive | exclusive | exclusive | prune apply |

Readers and a backup therefore run side by side. `gc` stays compatible
with readers: it unlinks only blobs no row references, and rows vanish
only under the exclusive mode.

A world folder holds `.sekai.lock` in the root the caller passed:
backup, status, and diff take it shared, rollback exclusive. Export
output stays unlocked, since it is new and empty by contract. A shared
world lock degrades to unguarded so read-only worlds stay readable (see
Consequences for the exact condition). `WorldTree::lock` defaults to
unguarded, so third-party trees keep compiling.

Locks are taken per operation, at each public `sekai-app` method's
entry. `open` never locks, and `init` locks only while it creates a
missing store, so an idle instance blocks nobody. Blocking passes that
write files or read blobs (backup ingest, rollback, export) own a share
of the locks, because a cancelled future cannot abort a blocking task
that already started. Read-only discovery passes need none: their
results are discarded with the future.

## Consequences

- Supersedes ADR-0011. `backup.inflight` is no longer written or read;
  one left behind by an older binary is inert and can be deleted. Old and
  new binaries do not exclude each other.
- `status` and `diff` may create `.sekai.lock` in the world root, which
  amends ADR-0010's "never writes" to "never writes region data".
- Cancellation safety covers the blocking file passes only. A library
  caller that drops a future during a metadata commit (backup, prune)
  releases the lock while the database may still finish that commit. The
  CLI is unaffected: Ctrl-C ends the whole process.
- A shared world lock degrades to unguarded only when its file is absent
  and cannot be created (permission denied, read-only filesystem). An
  existing lock file the user cannot open is an error.
- The store lock never degrades: every lock file opens for writing, so a
  store on a read-only mount fails every operation, reads included.
- WASI has no std file locks (`try_lock` returns `Unsupported`), so every
  operation fails there. This is deferred: `storage`/`world`/`app` are not
  wasm targets, and porting them also needs SQLite and a threaded runtime
  on wasm, so locking gets redesigned together with that port.
- Separate operations are not atomic together. The CLI resolves a
  snapshot reference, then runs the command, and a `prune` can slip in
  between. Commands that check snapshot existence fail loudly; known gaps
  are tracked separately.
- `init` skips the lock once `meta.sqlite` exists, and so does `open`. A
  caller arriving while another is still creating the schema therefore
  fails with an SQL error instead of `StoreLocked`; nothing is corrupted.
  This is the same pre-existing schema-creation race as `open` versus a
  first `init`, tracked separately.
- Two roots naming the same world (a Bukkit server root versus one of its
  world folders) get different lock files and do not exclude each other.
- `SekaiInstance::store()`/`store_mut()` bypass locking, as escape
  hatches. Network filesystems may not honor these locks. A file lock
  cannot coordinate across hosts, so future `mysql`/`postgres` backends
  need their own mechanism.
- Coordination with the Minecraft server stays out of scope.
