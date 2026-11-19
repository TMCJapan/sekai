# Checkout

```sh
sekai checkout experiment                  # switch to a branch
sekai checkout v1                          # detach HEAD at a commit
sekai checkout main~2 --in overworld       # restore an area, HEAD stays
sekai checkout -f                          # discard uncommitted changes
```

`checkout` switches HEAD or restores part of the world, as Git's does:

- A branch attaches HEAD to it (`Switched to branch 'experiment'`). A
  branch checked out in another worktree is refused.
- Any other commit-ish, such as a tag, a commit ID, or `main~2`,
  detaches HEAD, with Git's detached HEAD advice when HEAD leaves a
  branch.
- With a selection (`--in`, `--region`, `--kind`, or `--scope`), only
  that area is rewritten from the commit-ish, which defaults to HEAD.
  HEAD does not move.
- Alone, `checkout` changes nothing; `checkout -f` rebuilds the whole
  world from HEAD. Both fail on a branch with no commit yet.

The world is rebuilt from the target's commit before HEAD moves, region
files atomically (temp file in the target directory + `fsync` +
`rename`; in-place mutation never happens). A region file that HEAD's
commit and the target's agree on is left alone, so its uncommitted
changes carry over.

## Uncommitted changes

Without `-f`, `checkout` refuses before touching anything when it would
lose a chunk added or changed since HEAD's commit:

```text
error: your local changes to 2 region files would be overwritten by checkout
```

Commit them first, or pass `-f` to discard them. A chunk deleted since
HEAD does not count, since restoring it loses nothing.

## Restore policy

The default policy is strict:

- region files unknown to the commit are deleted;
- chunks unknown to the commit vanish on rebuild;
- tombstoned chunks are removed (fully tombstoned regions delete the
  file, never leaving a header-only shell);
- a blob missing from CAS aborts loudly (corruption — a partial world
  would be worse than none).

Each behavior is configurable without changing the default:

| Flag | Effect |
|---|---|
| `--keep-post-commit-files` | keep commit-unknown files instead of deleting them |
| `--keep-post-commit-chunks` | keep live bytes for commit-unknown chunks inside rebuilt regions |
| `--keep-tombstoned-chunks` | keep live bytes for tombstoned chunks; fully tombstoned files are left alone |
| `--on-missing-blob skip-chunk` | skip chunks whose blob is missing (default `abort`) |
| `--on-missing-file derived-only\|error` | ignore sibling folders, or fail, instead of guessing a location (default `sibling-first`) |

- `-j`/`--jobs N` sets how many files rebuild at once (`0` means one per
  CPU).
- Scope flags rebuild and delete only inside the scope; see
  [Scope selection](../scope.md). Combine with `--timing`/`--json` as
  usual.
