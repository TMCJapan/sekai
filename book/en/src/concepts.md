# Concepts

- **Commit**: one recorded world state, identified by a content-addressed
  Blake3 hash over its parents, creation time, and delta (`log` shows
  them oldest first, using the store's local record order). Commits store
  only fresh rows; reads resolve effective state through fallback, so
  metadata grows with changes, not with universe size. Recording the same
  commit twice is idempotent.
- **Ref**: a named, movable pointer to a commit - sekai's branch. One
  ref is active, and new commits hang from it; switching the active ref
  forks the history. See [Refs](commands/refs.md).
- **Chunk hash (CAS key)**: the normalized digest of a decompressed chunk,
  over a canonical encoding with volatile tags such as `LastUpdate`
  excluded. It is independent of key order and compression codec.
  `entities`/`poi` payloads have no level-chunk schema, so they key on
  their exact bytes. Identical content shares one stored copy across all
  commits; checkout restores the captured bytes verbatim, rewinding
  volatile tags to capture time.
- **No separate diff hash**: `diff` compares the very same digest recorded
  at commit time, read straight from history. A payload that cannot be
  normalized gets no key and fails ingest instead of being stored under a
  byte-level identity.
- **Tombstone**: an explicit "chunk absent" row. A chunk deleted between
  commits is recorded as a tombstone, not as silence — this is what lets
  checkout delete it again faithfully.
- **No derived state table**: region fingerprints are not persisted. Every
  commit rereads in-scope files and writes rows only for chunks whose blob
  changed, so history stays correct without a cache to rebuild.

The authoritative definitions are in
[ARCHITECTURE.md](https://github.com/tmcjapan/sekai/blob/main/ARCHITECTURE.md) ("Data & Hashing Model" and
"History, State, and GC Model").
