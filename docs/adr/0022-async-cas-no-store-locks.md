# ADR-0022: The CAS is async and the store takes no lock

- Status: Accepted
- Date: 2026-11-01

## Context

ADR-0014 gave the store two lock files (`writer.lock`, `store.lock`) and
took them per operation. That bought exclusion the CAS and metadata did
not actually need:

- Blobs are content-addressed and immutable. A second `put` of the same
  hash is a no-op, and a reader never observes a torn blob (temp file +
  rename). Concurrent writers of *different* blobs touch different paths.
- Metadata conflicts resolve transactionally at commit time, and
  content-addressed commit IDs keep concurrent backups from colliding.
- `gc` already keeps to its own run marker (ADR-0011), which is the one
  pair that genuinely races: a commit stores blobs before it records rows.

The locks cost real work anyway. Every operation paid two filesystem
locks, and the store lock had to be carried into blocking tasks so a
cancelled future could not release it early (ADR-0014). That made the
lock a `'static` value threaded through every file pass.

The second problem was shape: `BlobStore` was async while
`BlockingBlobStore` mirrored it synchronously for ingest workers. Every
operation had to pick a side, and backends had to implement both.

## Decision

Operations never lock the store. `writer.lock`, `store.lock`, the `lock`
module, `StoreLock`/`StoreLockMode`, `StorageError::StoreLocked`,
`SekaiStore::lock`, and `BlockingBlobStore` are removed.

Blob adapters implement only the async `BlobStore`. `FileCas` performs its
filesystem work on `tokio::task::spawn_blocking`, and it is `Clone`: the
CAS is append-only, so ingest workers share it freely. Metadata stays
synchronous and transactional, with no lock around it.

Exclusion is per world, through `WorldTree::lock` (ADR-0014). A blocking
file pass owns its share of that lock *inside* the blocking task, so a
cancelled future cannot release the world while files are still being
written. Read-only passes share it through `SharedWorldGuard`; an
exclusive pass passes an `ExclusiveWorldGuard` into each step.

Because the file pass no longer owns a `'static` progression lock,
progress callbacks for checkout are plain `FnMut` and run on the async
side, between file steps.

## Consequences

- An operation on a store no longer conflicts with another operation on
  the same store. Two backups, a backup and a prune, or a rollback and a
  prune can now run at the same time; correctness rests on immutability,
  transactions, and the `gc` run marker rather than on mutual exclusion.
- `sekai-app/tests/lock.rs` keeps only the world matrix: every
  operation states the world mode it takes, and nothing states a store
  mode.
- Backends implement one blob trait instead of two.
- Every blob operation pays a blocking-pool hop, including small ones.
  Ingest already ran on the blocking pool, so the net cost for commits is
  nil.
- Cancellation safety still covers the blocking file passes only. Dropping
  a future mid-metadata-transaction relies on the database finishing or
  rolling back the transaction.
