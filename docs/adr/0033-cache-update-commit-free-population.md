# ADR-0033: Commit-free cache population (`cache update`)

- Status: Accepted
- Date: 2026-11-17

## Context

ADR-0030 populates the region-image cache only at commit time, and
ADR-0031 consumes it in diff and the status preview. A cold or wiped
cache - after `cache clean` before a secondary backup, or on a freshly
initialized store - forces the full per-chunk decode path until the next
commit. Backup workflows want warm proofs without recording history, and
before the first commit there is no committed tree to confirm candidates
against at all.

The soundness constraint is unchanged (ADR-0029): a cached image must
have produced the region node hash it is stored under. The output
contract (ADR-0002) gains a payload: `cache` so far only had `clean`,
with no phases to time.

## Decision

`sekai cache update [--world PATH] [--jobs N]` with the standard
`--json`/`--timing`/`--progress` flags; `--world` defaults to the current
directory. The app API is `WorldHandleMut::update_cache`, returning a
report and per-phase timings.

- **Key by construction, not confirmation.** Every discovered file is
  read and its chunks normalized exactly as commit ingest does; the entry
  is keyed under `file_region_hash` of the file's own chunks - the region
  node a full commit of the unchanged file would name. The entry is sound
  by construction, so no committed-tree confirmation is needed and the
  run works on an unborn store. A later full commit confirms and re-puts
  the same entry idempotently (ADR-0030's gates still hold).
- **Cache-only writes, whole world only.** Nothing reaches the CAS or the
  metadata; no commit is recorded. There are no scope flags: a partially
  covered file cannot be keyed soundly, since its own chunks do not
  produce the merged node a scoped commit would name.
- **Retain the image from read through `put`.** There is no metadata
  commit between hashing and writing, so the bytes hashed are the bytes
  stored and no verifying re-read is needed. Retention is per file and
  per worker - each image drops right after its put - so memory stays
  bounded by the fan-out, unlike retention across the commit barrier
  (ADR-0030's rejected alternative), which would hold every candidate at
  once on top of the staged payloads.
- **Fail loudly.** Unlike the commit-time pass, this one is not
  best-effort: an unhashable payload fails the run with its coordinates,
  exactly like ingest, and a cache write or sync failure fails it too -
  updating the cache is the whole point of the command. Entries already
  written stay (append-only derived state); files still in flight are
  aborted.
- **Same concurrency shape as ingest.** A shared world lock keeps the
  read pass stable; bounded fan-out with `--jobs 0` meaning one worker
  per CPU; progress counts files as they complete.

The report carries `files`, `chunks`, `entries` (newly inserted),
`existing` (first-image-wins kept the original, including distinct files
whose identical chunk content collapses to one node), and `empty` (files
holding no chunks name no region node and cache nothing). Timings carry
`total`, `discover`, `ingest`, `hash` (a subset of ingest), and `cache`
(writes plus the advisory flush).

## Consequences

- Diff and the status preview can prove unchanged regions right after
  `init` or `cache clean`, without a commit in between; the commit-free
  pass never touches history.
- The cost is one full read-and-normalize pass over the world - the same
  chunk work as commit ingest, minus the CAS and metadata writes.
- Output contract (additive): the `cache` envelope gains the update
  payload, and `cache update` accepts `--timing`/`--progress` while
  `cache clean` still accepts neither. The flag matrix and payloads in
  `docs/json.md` are updated; golden tests pin both shapes.
- Out of scope: scoped updates, cache eviction, and skipping ingest
  during a real commit (ADR-0031).