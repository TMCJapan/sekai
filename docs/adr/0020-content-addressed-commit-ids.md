# ADR-0020: Commit IDs are content-addressed hashes

- Status: Accepted
- Date: 2026-10-24

## Context

Commit IDs came from SQLite's `AUTOINCREMENT`: a store-local counter,
unique only within that store. That is enough for a single local
history, but it cannot support remote or distributed repositories:

- Two stores recording the same logical commit assign it different IDs,
  so a sync protocol would need an ID translation table.
- An ID says nothing about the commit's content, so a peer cannot verify
  what it received.
- Merging histories from independent stores would require renumbering,
  rewriting refs, tags, and parent edges.

Git's model solves this by deriving the object id from the commit object.
Sekai commits differ from Git objects in one respect: `prune` folds a
retired commit's rows into a retained successor, so physical row
placement is not immutable. Identity must therefore cover the logical
delta recorded at commit time, not the rows a store happens to hold
later.

## Decision

`CommitId` is a 32-byte Blake3 digest rendered as 64 lowercase hex
characters. The hash input is domain-separated (`sekai-commit-v1\0`) and
canonical:

1. sorted, deduplicated parent IDs;
2. `created_at_ms` as big-endian bytes;
3. fresh history rows sorted by coordinate: dimension, kind, x, z, then
   the optional blob hash and optional diff hash (presence-tagged);
4. sorted removed region keys: dimension, kind, region x, region z.

Carried regions and derived fingerprints are excluded: they describe
storage representation and observation metadata, not the logical delta.
Sorting makes the digest independent of caller iteration order.

Each backend keeps a local record sequence (`seq` in SQLite) alongside
the hash. It orders the timeline for latest-commit lookup, effective-row
tie-breaking, pruning, and retirement. The sequence is never exposed as
an ID and never participates in the hash.

Recording a commit whose hash already exists is idempotent: the existing
ID is returned and no rows are duplicated. `create_commit` hashes an
empty delta; the commit path uses `apply_commit_incremental`, which
hashes the fresh entries and removed regions.

References accept the full 64-character hash or a ref name. A 64-character
hex spelling is resolved as a commit hash first and shadows a ref of the
same name. Abbreviated hashes are not accepted. The SQLite schema moves
to version 11 with `BLOB` IDs; the pre-release policy applies: old stores
are recreated, never migrated.

## Consequences

- Identical histories produce identical IDs across independent stores,
  which gives a future remote protocol a stable identity to exchange and
  a value to verify on receipt. Verification requires transferring the
  logical delta; a post-prune physical row set is not the hash preimage.
- Repeated commits collapse instead of accumulating rows.
- `prune` changes row placement without changing a retained commit's ID,
  because the ID covers the delta recorded when that commit was created.
- Ordering never uses hash order: hashes are uniformly distributed, so
  sorting by them would be meaningless. All timeline decisions use the
  backend's local sequence.
- The timestamp is part of the hash. Recording the same delta at a
  different time yields a different commit, just as a Git commit's
  timestamp changes its object id.
- Full hashes are long to type. Tags and refs are the human-readable
  handles; scripts should copy the hash from `log` or `--json`.

See [ADR-0019](0019-ref-objects.md) for the ref model, which now points
at these hashes.
