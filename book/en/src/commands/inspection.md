# Inspection

These commands never write to the world or the store.

```sh
sekai --store ./sekai-store list
sekai --store ./sekai-store status ./world
sekai --store ./sekai-store diff 1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b 2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a --in overworld:0,0
```

- `log` prints commits oldest first with raw Unix-millis timestamps
  (RFC 3339 rendering is human-output only). Tags pointing at each
  commit are shown alongside; see [Tags](tags.md). `--stat` appends
  per-commit change statistics (fresh/tombstone/new-blob/effective
  counts). An empty store prints a note to stderr and exits 0.
- `status` previews what a commit would record: the basis commit
  (`latest`, `null` when the store is empty), changed regions (new and
  deleted files split out), chunks that would be newly recorded,
  tombstones, and blobs absent from CAS. `clean` means nothing in scope
  differs from the latest commit; counts match a subsequent commit
  unless the world changes in between. There is deliberately no
  `commit --dry-run`: `status` is that command.
- `diff` compares chunk NBT between two commits, or between the live
  world (`--world`) and a commit. One `--in DIM:x,z` selection keeps
  the single-chunk output; several selections switch to grouped output,
  omitting chunks without differences. Scope flags (`--in`, `--region`,
  `--kind`) select the compared chunks; see
  [Scope selection](../scope.md). A chunk absent or tombstoned on a
  side diffs as an empty compound there — missing coordinates are never
  errors (see [ADR-0006](https://github.com/tmcjapan/sekai/blob/main/docs/adr/0006-diff-missing-as-empty.md));
  only corrupt payloads and missing blobs fail loudly. With commit
  arguments, a missing newer side defaults to the active ref's target and
  a missing older side defaults to the newer side's lowest-ID parent; when
  both are omitted, diff uses the active ref's target against its
  lowest-ID parent, so a root commit has no implicit older side and the
  command errors — pass the older commit explicitly or use `--world`
  (whose single commit defaults to the active ref's target). `--show-values`
  prints concrete old/new values in human output.
