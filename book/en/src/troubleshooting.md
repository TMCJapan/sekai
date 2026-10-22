# Troubleshooting

- `unknown commit: N` — the id does not exist. Check `log`; ids are
  never reused.
- `blob missing from CAS: <hex>` — store corruption (or a store copied
  incompletely). Checkout aborts rather than writing a partial world;
  `--on-missing-blob skip-chunk` (checkout/clone) continues without the
  affected chunks. Restore the store from the secondary copy, then
  investigate.
- `cannot derive region path for dim ...` (`UnknownRegionPath`) — a
  custom-dimension region whose folder is unknown, or
  `--on-missing-file error` refusing to guess. Run it against a world
  tree with the same folder layout the commit came from so sibling
  derivation can work, or accept that the region cannot be placed.
- `unsupported schema version` — the store was written by a newer (or
  older) binary. The project is pre-release: recreate the store, don't
  migrate.
- Region parse failures name the file (`failed to process region file
  ...`). Torn tails from interrupted saves are tolerated when
  unreferenced; genuinely corrupt sector runs fail loudly by design.
- Slow commands: rerun with `--timing` (release build) and compare
  phases; for commit-path slowdowns prefer `--timing --json`, which
  carries the per-region breakdown.
