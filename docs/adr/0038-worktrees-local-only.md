# ADR-0038: Worktrees live in the local backend

- Status: Accepted
- Date: 2026-11-22

## Context

A worktree is one world a store backs up, with its own HEAD (a branch
name or a detached commit). Putting worktrees on the backend-neutral
`MetaStore` trait and in `SekaiInstance::init` would leak a
host-filesystem concept into layers that exist to stay
platform-independent: `app` and below model the git-like history logic,
and only the local backend knows where a store or a world lives on
disk. Registering the init world by its path would also need an explicit
registration step (and a cwd capture in the CLI) that the store could
perform itself.

## Decision

Worktrees are owned by `sekai-storage-local` alone, keyed by store
paths:

- `sekai-util` carries only the pure value `Head` (branch name or commit
  ID) and `RefName::default_branch()`; no path-bearing worktree record
  crosses into the pure layers.
- The `worktrees` table lives in the local schema; `STORE_VERSION`
  bumps to 3. `path` is the absolute store location a worktree opens:
  the store root itself for the store's own (main) worktree, a store
  link file for a linked one. Every key resolves through
  `std::path::absolute` alone - symlinks stay unresolved, so the
  registered spelling is the identity a later open reproduces.
  `head_ref`/`head_commit` hold the HEAD, exactly one set; a partial
  unique index on `head_ref` keeps a branch checked out by at most one
  worktree. No `main` flag is stored.
- `LocalStore::init` registers the store's own row (absolute root,
  HEAD unborn on the default branch) with `ON CONFLICT DO NOTHING`, so
  a re-init keeps the row and its HEAD. No external registration call
  exists.
- `LocalStore::add_worktree(link, head)` writes the store link and
  registers the link path with the given HEAD.
- `sekai-app`, `sekai-storage`, and the CLI's init flow are unchanged.

## Consequences

The backend-neutral layers stay path-free, and a future backend defines
its own worktree surface (or none) without touching `core`/`app`. The
path a caller opens is the worktree key, so a later change can resolve
a worktree's HEAD directly from the location passed to open. The store
belongs to itself rather than to the world init ran in: initializing
from any directory is idempotent. Commits still land on the active ref;
worktree HEADs take over that role in a later change.
