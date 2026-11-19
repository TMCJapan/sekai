# ADR-0035: Turso replaces sqlx for commit metadata

- Status: Accepted
- Date: 2026-11-19

## Context

Metadata lived in SQLite through sqlx. The dependency forced a git pin
(`release/0.9.1` branch) to dodge RUSTSEC-2026-0459 in the bundled
`libsqlite3-sys`, and the async pool existed only to hop onto sqlx's
SQLite worker thread: the store already serialized every operation
behind one connection. Turso is an in-process, SQLite-dialect engine
with a native async Rust API (`turso` crate), so the same relational
schema and transactional guarantees can run without sqlx, without a
bundled SQLite, and without the worker-thread hop.

Constraints: the metadata schema, the version gate, and the safety
invariants (atomic schema creation, active-ref continuation checks,
idempotent commits, typed corruption errors) must survive the swap.
The project is pre-release: old stores are recreated, not migrated.

## Decision

- Replace sqlx with `turso` 0.8.3 (default features off: no `mimalloc`
  global allocator from a library, no unused `fts`). Drop the sqlx git
  patch and its `deny.toml` source allowance.
- `SqliteMeta` becomes `TursoMeta`: one `turso::Connection` instead of a
  one-connection pool. The connection keeps the database alive on its
  own; `busy_timeout(5s)`, `synchronous = FULL`, and `foreign_keys = ON`
  are pinned at connect, and WAL is the engine default. Connect retries
  busy failures for the same 5s window the old pool did.
- The SQL schema is unchanged. Rows decode by column index against the
  explicit SELECT lists; bulk node inserts/reads keep the 1024-row
  batches to bound one statement's SQL text and parameter vector.
  Schema creation still runs under `BEGIN IMMEDIATE` with the version
  stamp in the same transaction.
- Bump `SCHEMA_VERSION` 17 -> 18. Turso can parse a SQLite-written file
  (it reads the header and `user_version`), but the on-disk formats are
  not interchangeable, so a store stamped by the SQLite engine must be
  refused by the version gate, not opened. A gate test pins the refusal
  of version 17.
- The store layout keeps the `meta.sqlite` file name: it is the store's
  existence marker, and the book, CLI tests, and layout contract all
  name it. Only the engine behind it changes.

## Consequences

- No sqlx, no bundled SQLite, no git patch to carry; the advisory
  workaround disappears with the dependency. Metadata queries run
  in-process on the async runtime instead of crossing a worker-thread
  boundary.
- Stores written by the SQLite engine (version <= 17) fail with
  `UnsupportedSchema`; pre-release policy recreates them. There is no
  migration path, and none is planned.
- Turso is a young engine: its WAL and internal sequence tables are its
  own, so the file must not be opened by SQLite tooling, and engine
  upgrades need the same conformance suite run as any storage change.
- The corruption-simulation tests toggle `foreign_keys` /
  `ignore_check_constraints` on the store's single connection and
  restore them, instead of opening a dedicated raw connection; the
  guarantees under test are unchanged.
