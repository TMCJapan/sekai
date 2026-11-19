# Commit

```sh
sekai commit
```

Records the world as a new commit on HEAD. Like Git, it first prints a
summary line, `[main (root-commit) <id>]`, `[main <id>]`, or
`[detached HEAD <id>]`, then the chunk/blob counts and tombstones.
Recording an identical commit again returns the existing hash without
adding rows.

Before recording, the command runs the same pass as `cache update` over
the whole world, warming the region-image cache so `diff` and `status`
can prove unchanged regions right away (see [Cache](cache.md)). With
`--timing`, `cache_ms` measures that pass and the total spans both;
`--no-cache` skips it.

- `-j`/`--jobs N` sets the worker count for both passes (`0` means one
  per CPU).
- Progress shows on stderr when it is a terminal; `--no-progress` turns
  it off, and `--json` keeps it off.
- Scope flags (`--in`, `--region`, `--kind`) restrict what is recorded;
  see [Scope selection](../scope.md).
- For a dry-run preview of the above, see `status` under
  [Inspection](inspection.md).
- A commit has no message, so there is no `-m`.

Every region file in scope is read on each commit; unchanged chunks
resolve through fallback and cost no new blobs or history rows. A commit
with no changes still writes one commit row.
