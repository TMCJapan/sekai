# Inspection

These commands never write to the world or the store.

```sh
sekai status
sekai log --oneline --decorate
sekai log --all --stat
sekai diff --in overworld:0,0
sekai diff v1..main --in overworld:0,0..31,31
```

- `status` starts with Git's first line, `On branch <name>` or `HEAD
  detached at <id>`, where `<id>` is HEAD's commit, then previews what
  a commit would record: the basis commit, changed regions (new and deleted files split out), chunks that
  would be newly recorded, tombstones, and blobs absent from CAS.
  `clean` means nothing in scope differs from HEAD's commit; counts
  match a subsequent commit unless the world changes in between. There
  is deliberately no `commit --dry-run`: `status` is that command.
- `log` walks parents from HEAD, newest first, in Git's `medium` format
  without the author and the message, which sekai does not record.
  `--oneline` prints one line per commit, `--reverse` lists the oldest
  first, and `--all` starts from every ref. Revisions and ranges such as
  `v1..main` choose where to walk. Decorations such as `(HEAD -> main,
  tag: v1)` show on a terminal; `--decorate` and `--no-decorate` decide
  otherwise. `--stat` adds per-commit change statistics. A branch with
  no commit yet fails, as in Git.
- `diff` compares the world with HEAD, the world with one commit-ish,
  or two commits (`diff a b` or `diff a..b`). One `--in DIM:x,z`
  selection keeps the single-chunk output; several selections switch to
  grouped output, omitting chunks without differences. Scope flags
  (`--in`, `--region`, `--kind`) select the compared chunks; see
  [Scope selection](../scope.md). A chunk absent or tombstoned on a side
  diffs as an empty compound there — missing coordinates are never
  errors (see [ADR-0006](https://github.com/tmcjapan/sekai/blob/main/docs/adr/0006-diff-missing-as-empty.md));
  only corrupt payloads and missing blobs fail loudly. Comparing the
  world fails on a branch with no commit yet, as Git's `diff HEAD` does.
