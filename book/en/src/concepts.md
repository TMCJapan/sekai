# Concepts

- **Snapshot**: one recorded world state, numbered in creation order
  (`list` shows them oldest first). Snapshots store only fresh rows;
  reads resolve effective state through fallback, so metadata grows with
  changes, not with universe size.
- **Blob (CAS key)**: the exact raw chunk payload, hashed with framing
  included. Identical payloads share one stored copy across all
  snapshots; rollback restores these bytes verbatim, rewinding volatile
  tags such as `LastUpdate` to capture time.
- **Diff hash**: an opt-in volatile view over decompressed NBT with
  non-essential tags excluded. It speeds up change detection only and
  never alters stored blobs.
- **Tombstone**: an explicit "chunk absent" row. A chunk deleted between
  backups is recorded as a tombstone, not as silence — this is what lets
  rollback delete it again faithfully.
- **Derived state**: `region_state` fingerprints speed up the next backup.
  Wiping them costs at most one full ingest, never wrong data.

The authoritative definitions are in
[ARCHITECTURE.md](https://github.com/TMCJapan/sekai/blob/main/ARCHITECTURE.md) ("Data & Hashing Model" and
"History, State, and GC Model").
