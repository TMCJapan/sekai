# Tags

```sh
sekai --store ./sekai-store tag create stable 1
sekai --store ./sekai-store tag create stable @prev --force
sekai --store ./sekai-store tag delete stable
sekai --store ./sekai-store tag list
```

Tags give snapshots human-readable aliases so rollback targets stay
readable (`rollback ./world @stable` instead of an ID). Names use
`[A-Za-z0-9._-]`, run 1–64 bytes, and are never all digits, so `@123`
cannot be confused with snapshot `123`.

- Snapshot arguments to `rollback`, `export`, and `diff` accept `<id>`
  or `@tag`; reports always carry the resolved numeric ID. Malformed
  references are refused at parse time, before the store is touched.
- `tag create` fails over an existing name unless `--force` moves it.
  `tag delete` on a missing tag fails loudly.
- `tag list` lists all tags in name order; `list` shows each
  snapshot's tags alongside.
- Tags are metadata only and constrain nothing: pruning a tagged
  snapshot drops its tags with it.
