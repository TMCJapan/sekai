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

There is no snapshot pruning yet: GC only removes orphan blobs, never
metadata.

For a full secondary backup of everything — snapshots, metadata, and
blobs — copy or archive the entire store directory to an external
location. Per-command file writes are crash-ordered (blobs before
metadata commits, unlinks after), so a copied store is self-consistent.
