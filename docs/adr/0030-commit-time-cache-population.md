# ADR-0030: Commit-time population of the region-image cache

- Status: Superseded by ADR-0034
- Date: 2026-11-16

## Context

ADR-0029 added the region-image cache but nothing writes it: every entry
must be produced by a commit, since only the metadata commit names the
region hash an image may be stored under. The payoff ADR-0023 left open -
proving a fully covered region unchanged by whole-file comparison - needs
the cache populated for the commits it compares against.

Two constraints shape the population pass. Soundness: a cached image must
have produced the committed region node, so a partial-scope commit (whose
region node merges the parent's out-of-scope chunks) must not cache the
live file under the merged hash. Memory: ingest is streaming - each image
is dropped once its chunks are staged - so the raw bytes are no longer in
hand when the commit records.

## Decision

Commit populates the cache after `record_commit`, in three gates:

1. **Candidacy at ingest.** For every changed file the scope fully covers
   (`Scope::covers_region`), the blocking ingest pass keeps a candidate:
   the region key, the region node hash the file's own staged chunks
   produce (`file_region_hash`), and the `hash_bytes` digest of the raw
   image. The digest is computed while the bytes are already in hand, so
   candidacy costs no extra I/O and no retained image - 32 bytes per file.
   A preview (`status`) prepares nothing.
2. **Confirmation after the commit.** `cacheable_regions` (core policy)
   reads the committed root and the candidate dimensions in bulk and
   confirms a candidate only when the commit names exactly its hash for
   its region key. A mismatch means the merge carried state the file did
   not produce; the file is not cached.
3. **Re-read and verify.** Each confirmed file is re-read raw through the
   handle discovery found (never a re-derived path: non-canonical names
   like `r.00.00.mca` must resolve to the same file), with no chunk
   decode. Its content hash must still equal the ingest digest - a
   rewrite between the two reads leaves the entry absent - and only then
   does `cache.put(region, mca_hash, image)` run. First image wins, per
   ADR-0029.

The pass reuses ingest's bounded fan-out and holds the world lock for its
duration. It is fully best-effort: a tree read, file re-read, cache write,
or worker failure only leaves an entry absent and never fails the recorded
commit; the closing `sync` is advisory. `CommitTimings` gains a `cache`
phase (`cache_ms` in the CLI JSON) covering confirmation, re-reads, and
writes.

The `Region` trait gains `image() -> &[u8]`, exposing the undecomposed
bytes the cache stores; the host tree returns the parser's captured image.

### Alternative: retain images during ingest

Keeping every covered image in memory from ingest to commit avoids the
re-read, but a full commit would hold the world's entire `.mca` volume in
RAM (gigabytes for large worlds) on top of the staged payloads. The
re-read is one sequential raw pass per file with no chunk decode, and
memory stays bounded by the fan-out, so the streaming design wins.

## Consequences

- Every full commit caches every fully covered region; repeated commits
  re-confirm and re-put idempotently, so a wiped cache is rebuilt by the
  next commit even without world changes.
- The soundness chain is triple-gated: coverage at ingest, hash
  confirmation against the committed tree, and digest verification at
  re-read. A cached image always produced the region hash it is stored
  under.
- The cost is one extra raw read pass per covered file per commit plus
  the confirming tree reads; no chunk is decompressed or normalized
  twice.
- The consumer side - the file-level change check that skips unchanged
  regions - is implemented for diff and the status preview in
  [ADR-0031](0031-cache-query-region-proofs.md). Commit ingest is unchanged,
  so this population pass keeps every candidate.
