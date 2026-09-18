# Inspection

These commands never write to the world or the store.

```sh
sekai --store ./sekai-store list
sekai --store ./sekai-store diff 1 2 --in overworld:0,0
sekai debug scan ./world
```

- `list` prints snapshots oldest first with raw Unix-millis timestamps
  (RFC 3339 rendering is human-output only). Tags pointing at each
  snapshot are shown alongside; see [Tags](tags.md). `--stat` appends
  per-snapshot change statistics (fresh/tombstone/new-blob/effective
  counts).
- `diff` compares chunk NBT between two snapshots, or between the live
  world (`--world`) and a snapshot. One `--in DIM:x,z` selection keeps
  the single-chunk output; several selections switch to grouped output,
  omitting chunks without differences. A chunk absent or tombstoned on a
  side diffs as an empty compound there — missing coordinates are never
  errors (see [ADR-0006](../../docs/adr/0006-diff-missing-as-empty.md));
  only corrupt payloads and missing blobs fail loudly. `--show-values`
  prints concrete old/new values in human output.
- `debug scan` lists region files with size, mtime, chunk count, and
  header hash, plus per-phase timings with `--timing`.
