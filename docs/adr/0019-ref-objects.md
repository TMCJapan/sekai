# ADR-0019: Ref objects - named, movable pointers to commits

- Status: Accepted
- Date: 2026-10-22

## Context

ADR-0017 gave the history a graph: every commit names the commits it
continues. Until now a new commit continued *the latest commit of the
store*, which is a timeline, not a position: two histories in one store
could not be told apart, and nothing said where the next commit should
land.

Git solves this with ref objects - mutable names that point at a commit
(`refs/heads/main`) plus a symbolic `HEAD` naming the current branch.
A commit is recorded on the branch `HEAD` points at, and the branch
moves onto the new commit. Git keeps those pointers in files, not in
the object database, and they are the anchors a remote sync exchanges.

Sekai stores its metadata in a database, so the pointers belong there
too. A ref points at a commit ID. Like Git, that ID is a content hash
(ADR-0020): it covers the logical delta recorded at commit time, not the
physical rows a store happens to hold later. `prune` folding rows into a
successor changes placement, not the successor's ID.

The store must also answer "which ref is current" without scanning
rows, and the answer must survive a crash with the same atomicity as
the commit it belongs to.

## Decision

The metadata store carries a `refs` table:

```sql
CREATE TABLE refs(
    name TEXT NOT NULL PRIMARY KEY,
    commit_id BLOB NULL REFERENCES commits(id),
    active INTEGER NOT NULL DEFAULT 0 CHECK (active IN (0, 1)),
    created_at_ms INTEGER NOT NULL
);
CREATE UNIQUE INDEX idx_refs_active ON refs(active) WHERE active = 1;
```

- A ref is a **movable pointer** to a commit. `commit_id = NULL` means
  *unborn*: the ref names a history that has no commit yet.
- Exactly one ref is **active** (the partial unique index enforces it):
  the ref a new commit hangs from - sekai's `HEAD`.
- A fresh store starts with one active ref, `main`, unborn.
- `RefName` allows ASCII alphanumerics plus `.`, `_`, `-`, and `/`, up
  to 64 bytes, never all digits, never `HEAD` (reserved for the store's
  active ref), and no empty or traversing path component. A 64-character
  hex spelling is always resolved as a commit hash first, so it shadows a
  ref of that name.
- The active ref is **never removed**: a store without one would have
  nowhere to hang new commits from.

A new commit continues the target of the active ref, and the active
ref moves onto the new commit in the same transaction as the commit
itself - a ref that lags behind its history would fork the next commit.
Switching the active ref therefore forks the history: the new commit
continues the ref it was planned against, not the newest commit in the
store.

Where the active ref lives was considered against SQLite's two
application-owned header slots (`PRAGMA user_version`, taken by the
schema version, and `PRAGMA application_id`, conventionally a file-type
magic number). Both are 32 bits, so neither can hold a 32-byte commit
hash, and a header slot cannot be written together with the commit rows
it belongs to. The pointer lives in the table instead:
one row, one boolean, updated in the commit's own transaction.

Commit references accept a ref name as well: a 64-character hash,
`@tag`, or a bare ref name. `HEAD` stays reserved so the active ref is
never ambiguous.

Two consequences of the graph have to follow:

- **Fingerprints are per history.** `region_state` records which
  commit last confirmed a file. A fingerprint confirmed outside the
  active ref's history describes another branch, so it must not be
  carried into this one: the plan reads the fingerprints within the
  base commit's ancestry only. Carrying one across branches would
  record chunks the branch never had.
- **Pruning folds into a descendant.** A retired commit hands its rows
  to a retained commit that continues it, and a ref naming the retired
  commit follows the fold. A commit that several branches continue
  cannot be retired: folding it into one of them would strip the
  others of the rows they inherit through it, so `prune` refuses and
  names the commit.

## Consequences

A store holds several histories, and `sekai commit` lands on the
branch the operator selected. The cost is that switching branches
re-ingests the files whose fingerprints belong to the other branch
(they are confirmed outside the new history); the blobs deduplicate, so
only the read is repeated.

The `MetaStore` port carries the model: `set_ref`, `lookup_ref`,
`visit_refs`, `delete_ref`, `active_ref`, and `activate_ref`. The
commit path follows the same rule: `apply_commit_incremental` requires
the new commit to continue the active ref's target - a mismatched parent
list is rejected - and moves the active ref onto it inside the commit
transaction, so a ref can never lag behind the history it names. The core
use case reads the active ref as the base of a plan (`plan_commit`), so
the policy of *where* a commit lands stays in `core` while the atomicity
of the move stays in the backend.

The schema version goes from 9 to 10; the pre-release policy applies:
old stores are recreated, never migrated.
