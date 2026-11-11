# ADR-0024: Blobs live in a sled keyspace beside metadata

- Status: Accepted
- Date: 2026-11-11

## Context

ADR-0022 (blobs in a SQLite database) moved blobs out of the file CAS into
`blob.sqlite`, keeping the store a single directory. But the blob half paid
for machinery it never uses: blobs are opaque immutable payloads keyed by a
32-byte hash, with no queries, joins, or derived state. SQLite's WAL made
the durability barrier a checkpoint with space-management and retry
semantics (`sync` mapped to `wal_checkpoint(TRUNCATE)`), and a
one-connection sqlx pool serialized every blob operation behind one async
connection.

The blob port (ADR-0016) already isolates the representation, so the
backend can change without touching metadata code or use-case policy.
Metadata stays SQLite: it is relational and transactional, and its schema
gate is load-bearing.

## Decision

Blobs live in `<root>/blobs`, a sled keyspace beside `meta.sqlite`.
`SledBlob` replaces `SqliteBlob`; `LocalStore = Store<SqliteMeta, SledBlob>`.
The opener names (`init_sqlite`/`open_sqlite`) stay.

- **The default tree is the CAS.** Keys are the 32-byte chunk hashes,
  values the captured payloads. A reserved tree `__sekai_version` holds
  the format gate (key `schema`, an i64 little-endian): version 1,
  independent of the metadata `SCHEMA_VERSION`. An unknown version fails
  with `UnsupportedSchema`; the pre-release policy recreates, never
  migrates. There is no migration from `blob.sqlite` either.
- **`put` is insert-if-absent.** `compare_and_swap(key, None, payload)`
  reports `false` for an existing hash (ADR-0004 dedup) and never rewrites
  bytes under a live key.
- **`sync` is the batched durability barrier.** sled appends to its log
  without fsyncing per write; `sync` maps to `Db::flush`, which fsyncs the
  log. The port explicitly allows batching, and the app already runs the
  barrier per ingest worker and once more before the metadata commit, so
  the crash order is unchanged: blobs durable before rows name them.
- **`remove` is batched too.** A crash between `remove` and the next flush
  can resurrect a deleted blob; that is an orphan for the next GC, never a
  dangling reference (GC deletes after the metadata commit).
- **One opener per keyspace.** sled takes an exclusive session lock at
  open, system-wide: a second open of the same path fails while the first
  handle lives, even in the same process. Clones share the session, so
  ingest workers still share the CAS without exclusion. `open_sqlite`
  detects an initialized store by `blobs/conf` (written on first open)
  without taking the lock.
- **Every operation runs on the blocking pool.** sled is synchronous; each
  call is a `spawn_blocking` hop, the pattern ADR-0022 (async CAS, no store
  locks) established. `visit_blobs` collects the hashes in the blocking
  task (sled iterates in key order, the stable hash order GC expects) and
  visits them on the async side, since the visitor is not `'static`.

## Consequences

- The store stays one directory (`meta.sqlite` + `blobs/`), moved and
  backed up as a unit. The blob half no longer carries SQL, WAL
  checkpoints, or a connection pool; `sync` is a plain fsync barrier.
- The single-opener session lock curbs the cross-process concurrency that
  ADR-0022 (async CAS, no store locks) left open: two processes can no
  longer hold the same store at once, and an idle instance holds it until
  dropped. Operations still never lock; the exclusion is at open time.
  That ADR stays accepted for everything else (per-world exclusion, the
  GC run marker, the async CAS).
- Copying a live store must copy the whole `blobs/` keyspace (log and
  snapshots included) or run with no opener; `meta.sqlite` keeps its
  WAL-copy rule.
- Deleting a blob frees space inside the keyspace's files, which sled
  reuses as it recycles log segments; the keyspace does not shrink on
  demand, and no explicit VACUUM-style policy is needed yet.
- sled (MIT/Apache-2.0) is a new dependency confined to
  `sekai-storage-local`; the pure crates and `sekai-storage` are untouched.
  Its non-optional transitive deps need documented `deny.toml` exceptions:
  `instant` (BSD-3-Clause, RUSTSEC-2024-0384) and `fxhash`
  (RUSTSEC-2025-0057), both unmaintained but not vulnerable.
- Tests that opened a second handle on a live store (blob counts,
  metadata spot-checks) now read through the instance's own handle.