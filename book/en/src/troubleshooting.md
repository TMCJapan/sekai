# Troubleshooting

- `no store at <path>` — the store was never initialized. Run
  `sekai init` (with the same `--store`) first; only `init` creates a
  store, every other command fails without creating anything.
- `not in a sekai world` — no `.sekai` from the current directory up.
  Run the command inside a world, or pass `--world` (or `--store`).
- `this operation must be run in a work tree` — the command reads or
  writes a world, but `--store` named a store whose worlds the current
  directory is not in.
- `your local changes to N region files would be overwritten by
  checkout` — commit them, or pass `-f` to discard them.
- `'<branch>' is already used by worktree at '<path>'` — another
  worktree has the branch checked out; switch that worktree elsewhere
  first.
- `unknown commit: <hash>` — no commit with that hash exists in this
  store. Check `log` and copy the full 64-character hash. IDs are
  content-derived: the same parents, timestamp, and delta always produce
  the same hash, so an identical history in another store shares it.
- `blob missing from CAS: <hex>` — store corruption (or a store copied
  incompletely). Checkout aborts rather than writing a partial world;
  `--on-missing-blob skip-chunk` (checkout) continues without the
  affected chunks. Restore the store from the secondary copy, then
  investigate.
- `cannot derive region path for dim ...` (`UnknownRegionPath`) — a
  custom-dimension region whose folder the world's layout cannot map.
  Run it against a world tree with the same folder layout the commit
  came from so derivation (and sibling lookup) can work.
- `region file r.<x>.<z>.mca ... is missing ... refuses to derive its
  location` (`MissingRegionFile`) — `--on-missing-file error`: the commit
  rebuilds a region whose file no longer exists, and the `error` policy
  forbids guessing where to rebuild it. Run it against the original
  layout, or drop the flag to recreate the file at the derived location.
- `unsupported schema version` — the store was written by a newer (or
  older) binary. The project is pre-release: recreate the store, don't
  migrate.
- Region parse failures name the file (`failed to process region file
  ...`). Torn tails from interrupted saves are tolerated when
  unreferenced; genuinely corrupt sector runs fail loudly by design.
- Slow commands: rerun with `--timing` (release build) and compare
  phases; for commit-path slowdowns prefer `--timing --json`, which
  carries the per-region breakdown.
