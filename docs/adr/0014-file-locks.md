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
| writer | exclusive | shared | backup, gc plan/apply, store creation |
| exclusive | exclusive | exclusive | prune apply |

Readers and a backup therefore run side by side. `gc` stays compatible
with readers: it unlinks only blobs no row references, and rows vanish
only under the exclusive mode.

A world folder holds `.sekai.lock` in the root the caller passed:
backup, status, and diff take it shared, rollback exclusive. Export
output stays unlocked, since it is new and empty by contract. A shared
world lock degrades to unguarded when the file can be neither created
nor opened, so read-only worlds stay readable. `WorldTree::lock` defaults
to unguarded, so third-party trees keep compiling.

Locks are taken per operation, at each public `sekai-app` method's
entry, never in `open`/`init`; an idle instance blocks nobody. Blocking
file passes own a share of the locks, because a cancelled future cannot
abort a blocking task that already started.

## Consequences

- Supersedes ADR-0011. `backup.inflight` is no longer written or read;
  one left behind by an older binary is inert and can be deleted. Old and
  new binaries do not exclude each other.
- `status` and `diff` may create `.sekai.lock` in the world root, which
  amends ADR-0010's "never writes" to "never writes region data".
- Separate operations are not atomic together. The CLI resolves a
  snapshot reference, then runs the command, and a `prune` can slip in
  between. Commands that check snapshot existence fail loudly; known gaps
  are tracked separately.
- Two roots naming the same world (a Bukkit server root versus one of its
  world folders) get different lock files and do not exclude each other.
- `SekaiInstance::store()`/`store_mut()` bypass locking, as escape
  hatches. Network filesystems may not honor these locks. A file lock
  cannot coordinate across hosts, so future `mysql`/`postgres` backends
  need their own mechanism.
- Coordination with the Minecraft server stays out of scope.
