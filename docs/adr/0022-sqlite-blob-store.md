# ADR-0022: Blobs live in a SQLite database beside metadata

- Status: Accepted
- Date: 2026-10-30

## Context

The file CAS stored each blob as `<root>/blobs/ab/cdef...`: a sharded
directory tree of small immutable files. That layout worked, but it kept
two storage representations in one store. Metadata was a SQLite database
while blobs were loose files with their own fsync and directory-sync
discipline, their own sharding rule, and their own failure modes (a
rename races on Windows, an interrupted directory creation, a partially
written tree that `gc` must distinguish from corruption).

A store is already addressed as one directory and moved, copied, or
backed up as a unit. Two representations make that unit harder to reason
about: copying the directory while a commit runs can capture metadata
without the blobs it names, or blobs without the metadata that kept them
referenced. The metadata backend also already provides transactions,
durability, and a version gate, so the filesystem was reimplementing
parts of a database for the second half of the store.

ADR-0016 placed both the async `BlobStore` and the synchronous
`BlockingBlobStore` behind backend-neutral crates. That boundary made
the blob representation replaceable without touching metadata code, and
the store lock (ADR-0014) already excludes commits from `gc`, so the
old run-marker mechanism has no remaining job.

## Decision

Blobs live in `<root>/blob.sqlite`, a second SQLite database beside
`meta.sqlite`, with its own `PRAGMA user_version` gate. One
`WITHOUT ROWID` table holds them:

```sql
CREATE TABLE blobs (
    hash BLOB NOT NULL PRIMARY KEY CHECK (length(hash) = 32),
    payload BLOB NOT NULL
) WITHOUT ROWID;
```

- **The hash is the primary key.** Lookup, insertion, and removal are
  B-tree operations on the 32-byte key, with no sharded directory or
  rename protocol. `WITHOUT ROWID` stores the payload beside the key, so
  fetching a blob is one lookup rather than a lookup plus an indirection
  through a rowid.
- **The blob database has its own schema version.** It is independent of
  the metadata `SCHEMA_VERSION` (14 at the time of writing). Blob rows
  are opaque payloads with no derived state to reinterpret, so its gate
  (`blob.sqlite` version 1) only guards the storage format. The
  pre-release policy applies unchanged: an unknown version is refused, and
  the operator recreates the store rather than migrating it. There is no
  migration from the file-CAS layout either; a pre-`blob.sqlite` store is
  not opened.
- **Each `put` commits durably before metadata names it.** The database
  uses WAL journaling with `synchronous = FULL`, so an autocommit
  `INSERT` returns only after the WAL commit is fsynced. This preserves
  the crash order from the architecture: blobs are on disk before the
  metadata rows that reference them commit.
- **Duplicate puts deduplicate instead of racing.** `INSERT OR IGNORE`
  returns `false` when the hash already exists, matching ADR-0004's
  concurrent-put contract. The primary key now enforces uniqueness
  directly; the first row wins and its payload is identical by
  construction, so no racer rewrites bytes under an existing hash.
- **`remove` and `visit_blobs` are plain statements.** `remove` is a
  `DELETE` in autocommit mode, and `visit_blobs` walks
  `SELECT hash FROM blobs ORDER BY hash`, so GC sees a stable hash order.
  GC and commits already exclude each other through the store writer
  lock (ADR-0014), with no marker file.
- **`sync` checkpoints the WAL.** `BlockingBlobStore::sync_dirs` maps to
  `PRAGMA wal_checkpoint(TRUNCATE)`, folding the WAL back into the main
  database file and truncating it so it stays bounded between commits. A
  reader holding the WAL can leave the checkpoint incomplete; the WAL
  remains valid and the next sync retries the truncation.
- **One async connection, one blocking connection.** The async side uses
  a one-connection sqlx pool: SQLite serializes writers anyway, so a
  queue in front of one connection costs nothing. Schema creation runs
  under `BEGIN IMMEDIATE` with a short `SQLITE_BUSY` retry, because
  switching a new database into WAL needs an exclusive lock that the
  busy handler cannot wait on. The blocking side owns a second direct
  connection behind a mutex, so `spawn_blocking` workers do not borrow
  the async runtime.
- **The blocking side runs a small thread-parking executor.** The
  blocking trait is reached from ingest, checkout, and clone workers,
  including under a current-thread runtime where
  `tokio::task::block_in_place` panics and `Handle::block_on` would touch
  runtime drivers the worker must not drive. SQLite queries complete on
  sqlx's per-connection worker without a reactor, so the implementation
  polls the future and parks the thread until the worker wakes it. This
  uses only safe `std` (`Wake` plus `thread::park`), adds no dependency,
  and is deliberately limited to futures that never wait on tokio timers.
  The store lock is taken through the same `lock_store` path as before,
  so the trait-level lock contract does not change.

`init_sqlite` creates both database files; `open_sqlite` requires both and
reports `StoreMissing` otherwise, so opening a directory never silently
materializes half a store.

## Consequences

- A store is a directory of two SQLite databases. Moving, copying, or
  backing it up stays a directory-level operation, and the file-CAS
  sharding, temp-file rename, and directory-fsync code disappears.
- Blob reads and writes pay SQLite's B-tree and WAL path. Payloads are
  copied through `Vec<u8>` on the sqlx boundary rather than streamed from
  a file; for the 4 KiB sector payloads ingest stores, that cost is
  small, and a future large-payload backend can implement the same port.
- Deleting a blob frees pages inside `blob.sqlite` instead of unlinking a
  file. The database does not shrink automatically; freed pages are
  reused, and a store that must return space to the filesystem needs an
  explicit `VACUUM` policy later if that becomes a caller concern.
- The WAL is part of the store between checkpoints. Copying a live store
  must copy `blob.sqlite` together with its `-wal` and `-shm` siblings
  (or run with no writer), just as `meta.sqlite` already requires.
- A blob database with an unknown `user_version` fails loudly before any
  table is touched. Since the pre-release policy recreates stores, there
  is no migration path from file-CAS data; operators recreate the store.
- The separate version gate means metadata and blob formats can evolve
  independently. A blob-format change bumps only
  `crates/storage-sqlite/src/blob.rs`, without touching metadata rows.
- ADR-0004's dedup semantics survive unchanged; the platform-specific
  rename race that motivated it is gone.
