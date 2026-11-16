# Commit

```sh
sekai commit ./world
```

Records the current world state as a new commit and prints the
commit hash, chunk/blob counts, and tombstones. Recording an identical
commit again returns the existing hash without adding rows.

Before recording, the command runs the same pass as `cache update` over
the whole world, warming the region-image cache so `diff` and `status`
can prove unchanged regions right away (see [Cache](cache.md)). With
`--timing`, `cache_ms` measures that pass and the total spans both.

- `--jobs N` sets the worker count for both passes (`0` means one per CPU).
- `--progress` shows a stderr progress bar (refused with `--json`).
- Scope flags (`--in`, `--region`, `--kind`) restrict what is recorded;
  see [Scope selection](../scope.md).
- For a dry-run preview of the above, see `status` under
  [Inspection](inspection.md).
- The store must already exist: run `sekai init` first (see
  [Store location](../store.md)). A missing store fails without
  creating anything.

Every region file in scope is read on each commit; unchanged chunks
resolve through fallback and cost no new blobs or history rows. A commit
with no changes still writes one commit row.
