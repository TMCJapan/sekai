# Cache

```sh
sekai cache clean
sekai cache update --world ./world
```

`cache` manages the region-image cache, the speed-only keyspace inside
the store (`cache/`, beside `meta.sqlite` and `blobs/`). It holds the
undecomposed `.mca` image behind each region node hash so `diff` and the
`status` preview can prove a fully covered region unchanged without
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
- `cache update` hashes the live world's region files and populates the
  cache without recording a commit:

  ```text
  cached 3 new region image(s), 1 already present, 0 empty (4 files, 101 chunks)
  ```

  It reads and hashes every chunk exactly as a commit would, but writes
  only cache entries — no blobs, no metadata, no commit. Use it to warm
  the cache after `cache clean`, or before the first commit, so `diff`
  and `status` can prove unchanged regions right away. `--world` defaults
  to the current directory; `--jobs` sets the hashing worker count (`0`
  means one per CPU). The whole world is hashed: a corrupt or unsupported
  chunk fails the run loudly, and entries already written stay.
- Use `clean` to reclaim space. Commits and blobs are append-only and are
  never deleted, so the cache is the only part of the store you can
  shrink. Cleaning before a [secondary backup](../store.md) keeps the
  copy small; the restored store simply runs colder until its next
  commit (or `cache update`).
- The store must already exist: run `sekai init` first (see
  [Store location](../store.md)). A missing store fails without creating
  anything.
- `--json` reports the removed count as `entries` for `clean`, and
  `files`/`chunks`/`entries`/`existing`/`empty` for `update`; the full
  payload contract is [docs/json.md](https://github.com/tmcjapan/sekai/blob/main/docs/json.md).
  `update` accepts `--timing` and `--progress`; `clean` has no phases to
  time or items to count, so neither applies to it.