# Commit

```sh
sekai --store ./sekai-store commit ./world
```

Records the current world state as a new commit and prints the
commit hash, chunk/blob counts, tombstones, skipped regions, and carried
chunks. Recording an identical commit again returns the existing hash
without adding rows.

- `--with-diff` additionally derives volatile diff views alongside blobs
  (slower ingest; the hot path stays decode-free without it).
- `--jobs N` sets the ingest worker count (`0` means one per CPU).
- `--progress` shows a stderr progress bar (refused with `--json`).
- Scope flags (`--in`, `--region`, `--kind`) restrict what is recorded;
  see [Scope selection](../scope.md).
- For a dry-run preview of the above, see `status` under
  [Inspection](inspection.md).
- When the store directory is created by the commit, the CLI says so
  on stderr (human output only).

Unchanged regions are skipped by fingerprint; unchanged chunks inside
changed regions resolve through fallback and cost no new blobs. A commit
with no changes still writes one commit row.
