# ADR-0031: Cache-query region proofs for diff and status

- Status: Accepted
- Date: 2026-11-16

## Context

ADR-0029 built the region-image cache and ADR-0030 populated it at commit
time; the consumer side was left as a follow-up. World-vs-commit diff
re-reads and decodes region files per chunk, and the status preview ingests
every in-scope file even when nothing changed. For a clean world - the
common backup case - both pay the full chunk-decode cost although the cache
already holds everything needed to prove whole regions unchanged.

Constraints: the proof must not change diff or preview results, error
semantics, progress counts, or the output contract; a cache miss, a byte
mismatch, or any failure falls back to the existing per-chunk path. `core`
stays `no_std` (policy over the storage traits); the app owns the file I/O.
Commit ingest and cache population stay unchanged, so ADR-0030's
wiped-cache rebuild invariant holds.

## Decision

- **Core policy**: `commit_region_hashes(meta, commit, keys)` returns the
  region node hashes a commit names for the given keys, with one bulk read
  per tree level - the consumer-side counterpart of `cacheable_regions`.
  An unknown commit or an absent region simply has no entry.
- **Shared app proof**: `prove_regions_unchanged` queries the cached content
  hash per region, re-reads each live file raw on the blocking pool (bounded
  fan-out, each task holding a share of the world lock), and compares
  `hash_bytes` of the image. Proven regions' committed nodes are read back in
  one bulk call. Best-effort: every failure leaves the region unproven.
- **Diff (world vs commit)**: only the regions the requested coordinates
  span are proven. Both sides answer a proven coordinate from the committed
  region node - the digest is read, not recomputed - while the per-chunk
  loop, progress, ordering, and timings structure stay as they were; the
  proof pass counts under `blob_fetch`. Commit-vs-commit diff is untouched:
  it is already metadata-only.
- **Status preview**: the proof runs only under `dry_run`, against the base
  commit. A proven file stages its outcome straight from the committed
  node's entries filtered by the scope - the present coordinates keep
  assembly from tombstoning chunks still on disk - with zero work timed and
  an immediate progress step. The proof counts under `universe_load`.
- **Commit**: unchanged. Every in-scope file is still ingested, so the
  post-commit cache population keeps all its candidates.

Soundness: the cache key is the commit's own region hash, and ADR-0030's
triple gate guarantees the cached image produced exactly that node. Live
bytes equal to the cached image therefore hold exactly the node's chunks.
A partial scope's merged node confirms no candidate, so it has no cache
entry and can never be proven; a scope narrower than the file only filters
which proven entries are staged.

## Consequences

- A clean-world diff or preview skips every chunk decompression and
  normalization: one raw hash pass per region replaces the per-chunk decode.
- The proof costs one raw file read per region that has a cache entry, and
  only for regions the operation touches - a single-chunk diff never
  re-hashes the whole world.
- A wiped or cold cache degrades to the previous full path and nothing
  else; the preview still writes nothing, and the next commit rebuilds the
  cache (ADR-0030).
- Out of scope: skipping ingest during a real commit (it would entangle
  with cache population) and checkout (it already rebuilds from the CAS).
