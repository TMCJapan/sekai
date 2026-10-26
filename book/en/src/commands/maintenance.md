# Maintenance

```sh
sekai gc --dry-run
sekai gc
```

`gc` collects CAS blobs no commit references. Planning is read-only;
applying re-verifies candidates against fresh metadata before unlinking,
so the two-phase shape (`gc plan` then apply) holds even though the CLI
runs both in one invocation. `gc --dry-run` prints the plan only, and
combines with `--timing` (the plan phase is measured; `apply_ms` is `0`)
but not with `--progress`, which has no apply phase to report.

GC removes only orphan blobs, never metadata; commit deletion is
`prune` (fold-into-next-retained, then `gc` to reclaim).

`gc` refuses while a `commit` is running: a commit writes blobs before it
commits the rows that reference them, so collecting in that window would
unlink blobs the next commit still needs. The error names the
`commit.inflight` marker in the store root — delete it only if no commit is
actually running (a crashed one leaves it behind). `commit` itself is never
blocked by a stale marker. See
[ADR-0011](https://github.com/tmcjapan/sekai/blob/main/docs/adr/0011-backup-run-marker.md).

## Pruning commits

```sh
sekai prune --keep-last 10 --dry-run
sekai prune --keep-last 10
sekai prune --before main
```

`prune` deletes old commits. Retention keeps the newest `--keep-last`
N intersected with `--before` and newer (at least one selector is
required); a selection retaining nothing fails loudly instead of wiping
the store.

Deletion folds oldest-first: each retired commit moves its
still-effective rows onto the next retained commit and drops rows
already superseded there, so every retained commit restores exactly
as before. Pruning never
unlinks blobs — run `gc` afterwards to reclaim the dereferenced ones.
`--dry-run` prints the planned deletions only.

For a full secondary backup of everything — commits, metadata, and
blobs — copy or archive the entire store directory to an external
location. Per-command file writes are crash-ordered (blobs before
metadata commits, unlinks after), so a copied store is self-consistent.
