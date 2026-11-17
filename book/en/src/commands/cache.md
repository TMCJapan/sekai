# Cache

```sh
sekai cache clean
```

`cache` manages the region-image cache, the speed-only keyspace inside
the store (`cache/`, beside `meta.sqlite` and `blobs/`). It holds the
undecomposed `.mca` image behind each committed region node hash so a
later commit can prove a fully covered region unchanged without
decompressing or normalizing a single chunk (see
[Concepts](../concepts.md)).

The cache is derived state, never history: every entry can be recomputed
from the world and the snapshot tree.

- `cache clean` removes every cached entry and reports how many were
  removed:

  ```text
  cleared 12 cached region image(s)
  ```

  It is safe at any time. Cleaning never changes what a commit or
  checkout reads or writes; it only costs performance until the next
  commit rebuilds the entries it needs. Running it again on an empty
  cache reports `0` — the operation is idempotent.
- Use it to reclaim space. Commits and blobs are append-only and are
  never deleted, so the cache is the only part of the store you can
  shrink. Cleaning before a [secondary backup](../store.md) keeps the
  copy small; the restored store simply runs colder until its next
  commit.
- The store must already exist: run `sekai init` first (see
  [Store location](../store.md)). A missing store fails without creating
  anything.
- `--json` reports the removed count as `entries`; the full payload
  contract is [docs/json.md](https://github.com/tmcjapan/sekai/blob/main/docs/json.md).
  There are no phases to time or items to count, so `--timing` and
  `--progress` do not apply.
