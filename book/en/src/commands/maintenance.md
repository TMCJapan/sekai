# Maintenance

```sh
sekai --store ./sekai-store gc --dry-run
sekai --store ./sekai-store gc
```

`gc` collects CAS blobs no snapshot references. Planning is read-only;
applying re-verifies candidates against fresh metadata before unlinking,
so the two-phase shape (`gc plan` then apply) holds even though the CLI
runs both in one invocation. `gc --dry-run` prints the plan only, and
combines with `--timing` (the plan phase is measured; `apply_ms` is `0`)
but not with `--progress`, which has no apply phase to report.

GC removes only orphan blobs, never metadata; snapshot deletion is
`prune` (fold-into-next-retained, then `gc` to reclaim).

`gc` refuses while a `backup` is running: a backup writes blobs before it
commits the rows that reference them, so collecting in that window would
unlink blobs the next commit still needs. `prune` likewise refuses while
anything else uses the store, and nothing else starts while it runs;
`prune --dry-run` only reads and runs alongside readers and backups.
Refusals are immediate (`store is in use by another sekai operation`);
retry once the other command finishes. The locks live in `writer.lock`
and `store.lock` in the store root and are released by the OS even when
a command crashes, so there is nothing to clean up. A `backup.inflight`
file left by older versions is no longer used and can be deleted. See
[ADR-0014](https://github.com/tmcjapan/sekai/blob/main/docs/adr/0014-file-locks.md).

## Pruning snapshots

```sh
sekai --store ./sekai-store prune --keep-last 10 --dry-run
sekai --store ./sekai-store prune --keep-last 10
sekai --store ./sekai-store prune --before @stable
```

`prune` deletes old snapshots. Retention keeps the newest `--keep-last`
N intersected with `--before` and newer (at least one selector is
required); a selection retaining nothing fails loudly instead of wiping
the store.

Deletion folds oldest-first: each retired snapshot moves its
still-effective rows onto the next retained snapshot and drops rows
already superseded there, so every retained snapshot restores exactly
as before. Tags on deleted snapshots disappear with them. Pruning never
unlinks blobs — run `gc` afterwards to reclaim the dereferenced ones.
`--dry-run` prints the planned deletions only.

For a full secondary backup of everything — snapshots, metadata, and
blobs — copy or archive the entire store directory to an external
location. Per-command file writes are crash-ordered (blobs before
metadata commits, unlinks after), so a copied store is self-consistent.
