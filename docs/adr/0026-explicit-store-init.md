# ADR-0026: Explicit store initialization

- Status: Amended by ADR-0028 (`init` registers a world as the first
  worktree, `--store` defaults to `<world>/.sekai`, and no ref is
  seeded)
- Date: 2026-11-12

## Context

Store creation was a side effect of `commit`: it opened the store through
the init path, materializing an empty store whenever the root was missing,
while every other command failed on a missing store (issue #80). The
responsibilities were mixed end to end: `SekaiStoreOpener::init` was
documented as "open, creating when missing", the `StoreMissing` error
pointed users at running a commit, and a mistyped `--store` on a commit
silently created a fresh store instead of failing.

Git separates the two steps: `git init` is the only command that creates a
repository, and every other command refuses to run without one. Sekai's
git-style vocabulary (ADR-0018) already invites that model.

## Decision

Add `sekai init` as the only command that materializes a store, and make
every other command (including `commit`) open only:

- CLI: a new `init` subcommand runs `SekaiInstance::init`; `commit` switches
  to `SekaiInstance::open`. Like `git init`, re-initializing an existing
  store is idempotent and reports itself as a re-initialization.
- Errors: `StorageError::StoreMissing` now reads
  `no store at <path>: run 'sekai init' to create one`.
- Library: `init`/`init_local` stay the only creation paths, now documented
  as reserved for an explicit init. A new `local_store_exists` predicate
  (re-exported by `app` under `backend-local`) checks the store markers
  without opening or locking anything, so `init` can tell a fresh creation
  from a re-initialization.
- JSON contract (ADR-0002): the envelope's `command` gains the value `init`;
  its payload is `{"store": "<path>", "created": <bool>}` (`created` is
  `false` on a re-initialization). `init` accepts `--json` and has no
  phases, so no `--timing`.

## Consequences

Breaking change: scripts that relied on `commit` creating the store must
run `sekai init` first; a commit on a missing store now fails without
creating anything. The CI smoke tests follow.

No schema change: an initialized store is exactly what a first commit used
to leave behind (metadata schema plus the version-gated blob keyspace, one
unborn active ref `main`), so existing stores keep working untouched.
