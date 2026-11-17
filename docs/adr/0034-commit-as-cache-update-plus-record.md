# ADR-0034: Commit as cache update plus record

- Status: Accepted
- Date: 2026-11-17

## Context

ADR-0030 hid cache population inside `WorldHandleMut::commit`: ingest
kept a candidate per fully covered file, and a best-effort pass after
`record_commit` confirmed each candidate against the committed tree,
re-read the file raw, and verified the digest before putting the image.
ADR-0033 then added `WorldHandleMut::update_cache`, which keys every file
under the region node its own chunks produce - sound by construction,
with no committed tree needed. Two population paths with two different
soundness arguments now did one job, and `sekai commit` was silently
`commit + cache update` while the app API pretended it was only commit.

## Decision

Split the command into its two operations and run them in order:

- **`WorldHandleMut::commit` records history only.** The candidacy,
  confirmation, and re-read machinery is removed from commit ingest, and
  `core::usecase::commit::cacheable_regions` goes with it. `CommitTimings`
  loses its `cache` phase; a commit writes no cache entry.
- **`WorldHandleMut::update_cache` is the only population path**
  (ADR-0033 unchanged).
- **`sekai commit` runs `update_cache` then `commit`**, sequentially, each
  under its own shared world lock. The update keys each file under the
  node its own chunks produce; on a stable world the commit that follows
  names exactly those hashes for the fully covered files, so the entries
  are immediately usable by diff and the status preview. A rewrite
  between the passes only leaves an entry the commit does not name -
  sound, just cold.
- **Output contract keeps its shape.** The commit payload still carries
  `cache_ms`, now measuring the pre-commit update pass, and `total_ms`
  spans both passes. The per-region rows still describe commit ingest
  only.

## Consequences

- One population path, one soundness argument: an entry's image always
  produced the region hash it is stored under, by construction. The
  triple gate (coverage, confirmation, digest re-verification) and the
  extra raw re-read pass per covered file are gone.
- Library callers choose: `commit` alone is history-only; a warm cache
  costs an explicit `update_cache` call, exactly as the CLI composes it.
- The cost is chunk work, not I/O: the update pass normalizes every
  chunk of every file, and commit ingest normalizes the changed files
  again - two normalization passes where ADR-0030 paid one plus a raw
  re-read. Scoped commits also hash the whole world, since
  `update_cache` has no scope, and fail loudly on a corrupt chunk
  anywhere in it, even outside the commit's scope.
- A partial-scope commit names a merged node the file did not produce
  whenever an out-of-scope chunk changed; that node stays uncached. When
  the file did produce the merged node, caching it is sound.
- Supersedes [ADR-0030](0030-commit-time-cache-population.md);
  [ADR-0033](0033-cache-update-commit-free-population.md) stands, now as
  the sole population design. Out of scope: scoped cache updates and
  skipping commit ingest for proven-unchanged files (ADR-0031).