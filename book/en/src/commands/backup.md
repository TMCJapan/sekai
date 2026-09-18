# Backup

```sh
sekai --store ./sekai-store backup ./world
```

Records the current world state as a new snapshot and prints the
snapshot id, chunk/blob counts, tombstones, skipped regions, and carried
chunks.

- `--with-diff` additionally derives volatile diff views alongside blobs
  (slower ingest; the hot path stays decode-free without it).
- `--jobs N` sets the ingest worker count (`0` means one per CPU).
- `--progress` shows a stderr progress bar (refused with `--json`).
- Scope flags (`--in`, `--region`, `--kind`) restrict what is recorded;
  see [Scope selection](../scope.md).
- For a dry-run preview of the above, see `status` under
  [Inspection](inspection.md).

Unchanged regions are skipped by fingerprint; unchanged chunks inside
changed regions resolve through fallback and cost no new blobs. A backup
with no changes still writes one snapshot row.
