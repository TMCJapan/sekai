# Tags

```sh
sekai --store ./sekai-store tag create stable 1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b
sekai --store ./sekai-store tag create stable @prev --force
sekai --store ./sekai-store tag delete stable
sekai --store ./sekai-store tag list
```

Tags give commits human-readable aliases so checkout targets stay
readable (`checkout ./world @stable` instead of a 64-character hash).
Names use `[A-Za-z0-9._-]`, run 1–64 bytes, and never consist of
digits alone.

- Commit arguments to `checkout`, `clone`, and `diff` accept a
  64-character lowercase hexadecimal commit hash or `@tag`; reports
  always carry the resolved hash. A 64-character hex spelling resolves
  as a commit hash first, so it shadows a ref of that name. Malformed
  references are refused at parse time, before the store is touched.
- `tag create` fails over an existing name unless `--force` moves it.
  `tag delete` on a missing tag fails loudly.
- `tag list` lists all tags in name order; `log` shows each
  commit's tags alongside.
- Tags are metadata only and constrain nothing: pruning a tagged
  commit drops its tags with it.
