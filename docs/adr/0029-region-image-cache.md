# ADR-0029: A region-image cache keyed by region node hash

- Status: Accepted
- Date: 2026-11-15

## Context

ADR-0023 dropped the fingerprint pass but left a file-level change check
open for tree-structured history: an unchanged region should reuse its
previous subtree by hash, comparing against a local cache and only for
files the scope fully covers. The snapshot tree (ADR-0027) now gives every
region a stable content hash - the `region_tree` primary key - but proving
a live file still matches a commit's region hash today means decompressing
and normalizing every chunk in it.

The cheap proof is a whole-file comparison: if the live `.mca` bytes are
identical to the bytes that produced the commit's region node, the region
is unchanged, no chunk decode required. That needs the undecomposed image
that produced a given region hash, plus its content hash so the comparison
runs without reading the image. Neither is derivable from the CAS, which
holds decomposed chunk payloads, not the file they came from.

## Decision

A third store adapter, the cache, maps a `RegionHash` to the undecomposed
`.mca` image that produced it and that image's `hash_bytes` content hash.

- **Trait**: `sekai-storage` owns `CacheStore` beside `BlobStore`/`MetaStore`
  (RPITIT, `Send` bounds): `put(region, mca_hash, image) -> bool`
  (insert-if-absent), `mca_hash(region) -> Option<[u8; 32]>`,
  `fetch_image(region, &mut out) -> bool`, and `sync`. `SekaiStore` gains a
  `Cache` associated type with `cache()`/`cache_mut()`; `Store` composes
  three adapters.
- **Local backend**: `SledCache` lives in `<root>/cache`, a sled keyspace
  beside `blobs/`. The default tree maps the 32-byte region hash to the raw
  image; a side tree `mca_hashes` maps it to the 32-byte content hash, so a
  hash lookup reads 32 bytes, never the image. A reserved `__sekai_version`
  tree holds the format gate (version 1, independent of the metadata and
  blob gates). `put` writes the (image, hash) pair in one cross-tree sled
  transaction so a reader that sees one sees the other; the first image
  wins, since different raw files can normalize to the same region node.
  Every operation runs on the blocking pool; one opener per keyspace.
- **Derived, never truth**: entries are written after the metadata commit
  names their region hash, and a miss falls back to the full per-chunk
  path. The cache records no absence and never decides a commit on its own,
  so a torn write or a wiped cache costs performance only. `sync` is
  advisory, not a crash-order barrier. The source-of-truth schema is
  unchanged.

## Consequences

- The file-level change check ADR-0023 left open now has its data: given a
  commit's region hash, read the cached content hash, hash the live file,
  compare. The consumer side is wired into diff and the status preview by
  [ADR-0031](0031-cache-query-region-proofs.md); commit ingest still reads
  every file, so cache population keeps its candidates.
- The store gains a third keyspace (`meta.sqlite` + `blobs/` + `cache/`),
  still one directory moved and backed up as a unit. `open_local` detects
  an initialized store by `cache/conf` too; a pre-cache store fails
  `StoreMissing` and is recreated, per the pre-release policy.
- The "no persisted derived state" invariant is restated as "no derived
  state in the source of truth": the cache is derived but lives outside the
  metadata schema and can always be rebuilt, so it cannot drift.
- sled stays confined to `sekai-storage-local`; the pure crates and
  `sekai-storage` are untouched. The cache stores whole images, so it is
  larger than the deduplicated CAS; it is rebuildable, and a future
  eviction policy can bound it.
