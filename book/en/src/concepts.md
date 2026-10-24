# Concepts

- **Commit**: one recorded world state, identified by a content-addressed
  Blake3 hash over its parents, creation time, and delta (`log` shows
  them oldest first, using the store's local record order). Commits store
  only fresh rows; reads resolve effective state through fallback, so
  metadata grows with changes, not with universe size. Recording the same
  commit twice is idempotent.
- **Blob (CAS key)**: the exact raw chunk payload, hashed with framing
  included. Identical payloads share one stored copy across all
  commits; checkout restores these bytes verbatim, rewinding volatile
  tags such as `LastUpdate` to capture time.
- **Diff hash**: an opt-in volatile view over decompressed NBT with
  non-essential tags excluded. It speeds up change detection only and
  never alters stored blobs.
- **Tombstone**: an explicit "chunk absent" row. A chunk deleted between
  commits is recorded as a tombstone, not as silence — this is what lets
  checkout delete it again faithfully.
- **Derived state**: `region_state` fingerprints speed up the next commit.
  Wiping them costs at most one full ingest, never wrong data.

The authoritative definitions are in
[ARCHITECTURE.md](https://github.com/tmcjapan/sekai/blob/main/ARCHITECTURE.md) ("Data & Hashing Model" and
"History, State, and GC Model").
