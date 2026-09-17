# Tags

```sh
sekai --store ./sekai-store tag stable 1
sekai --store ./sekai-store tag stable @prev --force
sekai --store ./sekai-store tag -d stable
sekai --store ./sekai-store tag
```

Tags give snapshots human-readable aliases so rollback targets stay
readable (`rollback ./world @stable` instead of an ID). Names use
`[A-Za-z0-9._-]`, run 1–64 bytes, and are never all digits, so `@123`
cannot be confused with snapshot `123`.

- Snapshot arguments to `rollback`, `export`, and `diff` accept `<id>`
  or `@tag`; reports always carry the resolved numeric ID.
- Creating over an existing name fails unless `--force` moves it.
  Deleting a missing tag fails loudly.
- Bare `tag` lists all tags in name order; `list` shows each
  snapshot's tags alongside.
- Tags are metadata only and constrain nothing: pruning a tagged
  snapshot drops its tags with it.
