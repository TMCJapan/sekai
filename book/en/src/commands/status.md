# Status

```sh
sekai --store ./sekai-store status ./world
```

Previews what a backup would record, without writing anything: no CAS
puts, no metadata commit. Read-only against both world and store.

- Reports the basis snapshot (`latest`, `null` when the store is
  empty), changed regions (new and deleted files split out), chunks
  that would be newly recorded, tombstones, and blobs absent from CAS.
- `clean` means nothing in scope differs from the latest snapshot.
  Scoped runs report cleanliness within the scope only.
- `--jobs N` sets the preview worker count (`0` means one per CPU);
  diff views are always skipped (preview-irrelevant decode work).
- Counts match a subsequent backup unless the world changes in between.

There is deliberately no `backup --dry-run`: `status` is that command.
