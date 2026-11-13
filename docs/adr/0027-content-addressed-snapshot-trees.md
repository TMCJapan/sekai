# ADR-0027: Commits name content-addressed snapshot trees

- Status: Accepted
- Date: 2026-11-12

## Context

`chunk_history` stores deltas: a commit records only changed chunks and
tombstones, and effective state resolves through the parent graph (a
recursive ancestor CTE plus a window function per read). The model
taxes every flow:

- `lookup_chunk` / `visit_commit_chunks` pay the ancestry per query.
- `gc plan` materializes the effective chunk set of every commit.
- `prune` must fold a retired commit's rows into a retained successor
  (ADR-0008), which forces commit IDs to cover the logical delta rather
  than the stored rows (ADR-0020).

ADR-0023 already anticipates the replacement: each commit names a
complete tree, and an unchanged region reuses its previous subtree by
hash. Constraints: canonical encodings stay in the `no_std` layers,
backends stay pluggable behind the ports, the crash order holds, and
the pre-release schema policy recreates stores instead of migrating.

`gc` and `prune` were removed before this decision landed: commits and
blobs are append-only and the store never deletes either. The tree
model below therefore ships without them; ADR-0008's retirement
machinery is gone outright rather than reworked.

## Decision

### Snapshot trees

A commit names one root hash instead of owning chunk rows. Three Merkle
levels below it, all Blake3 through `hash_bytes` with distinct domain
tags (`sekai-root-tree-v1\0`, `sekai-dim-tree-v1\0`,
`sekai-region-tree-v1\0`):

| Node   | Entry (fixed-width, big-endian)                    | Keyed by    |
| ------ | -------------------------------------------------- | ----------- |
| root   | dim id `i32` + `DimHash`                           | dimension   |
| dim    | kind `i32` + rx `i32` + rz `i32` + `RegionHash`    | region file |
| region | local cx `u8` + cz `u8` + `ChunkHash`              | chunk slot  |

Entries sort by coordinate; the sorted array is the node's canonical
encoding and its hash preimage after the domain tag. Dim entries carry
the region kind, so `region`/`entities`/`poi` families never share a
slot. Leaves are the existing `ChunkHash` digests already keyed in CAS:
trees add no second chunk-hashing layer.

Absence is the tombstone: a chunk missing from its region node is gone,
and a region missing from its dim node deletes the file at checkout.
Empty nodes are never written; a fully emptied region simply leaves the
dim node.

Scoped commits merge the parent tree with the in-scope changes:
out-of-scope subtrees are reused by hash, never re-read. The
fingerprint cache ADR-0023 kept applies to files the scope fully
covers.

### Storage: one table per level, bytes as written

SQLite gains one table per level; `chunk_history` goes away, and
`commits` gains `root_hash BLOB NOT NULL REFERENCES root_tree(hash)`:

```sql
CREATE TABLE root_tree(
    hash BLOB NOT NULL PRIMARY KEY,
    node BLOB NOT NULL  -- canonical entry array; hash = H(tag || node)
);

CREATE TABLE dim_tree(
    hash BLOB NOT NULL PRIMARY KEY,
    node BLOB NOT NULL
);

CREATE TABLE region_tree(
    hash BLOB NOT NULL PRIMARY KEY,
    node BLOB NOT NULL
);
```

The stored bytes are the hash preimage, so verification is one hash
call and can never drift from row order. Writes are `INSERT OR IGNORE`,
children before parents, in the same transaction as the commit row;
unchanged subtrees are shared by hash and not re-inserted. The
commit-to-root foreign key anchors every commit to an existing tree.
There are no keys between levels: the edges live inside the blobs, a
cascade would be wrong on shared nodes anyway, and nodes are
append-only, so reachability is never recomputed.

Each level has its own hash type (`RootHash`, `DimHash`, `RegionHash`)
and its own table, and a node's entries name only the next level down:
root → dim → region → CAS leaves. A node cannot name its own level or
any level above it, so the reference graph is a layered DAG by
construction — a cycle is not expressible, and any walk terminates in
at most three hops. The codec lives in `core`; backends store opaque
bytes, so sibling backends need no relational tree logic.

Rejected: edge-row tables (`PRIMARY KEY(hash, child_hash)`) collapse
distinct regions whose trees are identical (empty regions dedup to one
hash), repeat the parent hash per row, and force canonical-order
reconstruction to verify a hash. Moving nodes into the CAS keyspace
instead is a viable follow-up but splits metadata atomicity across two
stores; SQLite keeps trees and the commit row in one transaction.

### Commit identity

The commit preimage becomes the domain tag (`sekai-commit-v2\0`),
sorted parents, `created_at_ms`, and the root hash — replacing
ADR-0020's delta preimage. Equal worlds at equal times produce equal
IDs and share the whole tree; recording twice stays idempotent. The
local `seq` keeps its ordering role.

With `prune` removed there is no retirement: a recorded commit is
immutable, its row never leaves the graph, and refs keep resolving to
the same snapshot tree. Nothing is folded or re-stamped.

### Reads

`lookup_chunk` is a path of primary-key probes (root, dim, region,
slot); `visit_commit_chunks` walks the tree; diff skips subtrees whose
hashes match. With `gc` removed, tree nodes and blobs are append-only
like the commits that name them: the crash order (blobs synced before
the metadata commit) means a torn commit leaves at most orphan nodes
and blobs, never dangling references, and nothing reclaims them.

The metadata schema bumps to version 16; old stores are rejected
(`UnsupportedSchema`) and recreated, per the pre-release policy.

## Consequences

- Reads and diff cost the tree path and the change set — not the
  ancestry or the universe. The ancestor CTEs disappear, and
  ADR-0020's delta preimage is superseded once this lands. ADR-0008's
  fold machinery is moot: `gc` and `prune` are removed, so nothing
  folds or sweeps.
- Tombstones stop being rows; absence in the tree is the record.
- Metadata write amplification is the price: one changed chunk rewrites
  its region node, dim node, and root. A dim node grows with its region
  count, so frequent commits on large worlds churn SQLite. If that
  bites, an intermediate area level bounds the fanout — a follow-up,
  not part of this decision.
- A snapshot no longer says which commit introduced a chunk; answering
  that needs a history walk (Git has the same property). The
  `MetaStore` surface reshapes accordingly: `record_chunk` and
  `visit_fresh_rows` go away, and `retire_commit` with them (`prune`
  is removed); fresh rows are a tree diff against the parent.
- Tree nodes and blobs are append-only: a torn commit leaves orphans
  that nothing reclaims, and the JSON contract (`docs/json.md`) loses
  the `gc` and `prune` payloads.
- Breaking: schema 16 rejects old stores, commit IDs change, and the
  port reshapes. Merges stay representable through `parents`; a tree
  merge policy is out of scope until something creates merges.
