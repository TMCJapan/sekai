# ADR-0023: Drop the fingerprint pass and its zero-valued output

- Status: Accepted
- Date: 2026-11-04

## Context

PR #151 removed the `region_state` table (schema 13): fingerprints are no
longer persisted, so no commit can skip an unchanged file or carry its
rows. The commit path kept computing them anyway. Before planning, every
discovered region file - out-of-scope files included - was read in full
and hashed (ADR-0012), and the result was dropped: only its size reached
the ingest worker balancing. Each in-scope file was then read a second
time for ingest.

The output kept the old contract alive with numbers that could no longer
move: `commit --json` always reported `"skipped_regions": 0` and
`"carried_chunks": 0`, and `--timing` reported a `fingerprint_ms` phase
for work nothing consumed.

Histories are expected to become tree-structured: each commit names a
complete tree instead of resolving through fold and fallback. There a
file-level change check is useful again - an unchanged region can reuse
its previous subtree by hash, much like Git's index skips rehashing a
file whose cached stat data still matches - but it will compare against
a local cache and only for files the scope fully covers. Nothing in
today's call site fits that shape.

## Decision

- Discovery reads metadata only: `RegionFile::size` (a `stat`) feeds the
  largest-first worker partition. No file content is read before ingest.
- `core::plan_commit` takes the discovered `RegionKey`s; the
  `Observation` wrapper is gone.
- `CommitReport`/`CommitTimings` lose `skipped_regions`,
  `carried_chunks`, and the `fingerprint` phase; `StatusTimings` loses
  `fingerprint`. The JSON fields `skipped_regions`, `carried_chunks`,
  and `phases.fingerprint_ms` are removed from `commit` and `status`,
  and the human `--timing` line drops `fp=`/`fingerprint=`,
  `skipped_files=`, and `carried_chunks=`.
- Kept for the tree-structured history: `RegionFingerprint`,
  `RegionFile::fingerprint`, `sekai_world::fingerprint_file`, and
  `Scope::covers_region`. ADR-0012 still governs how a fingerprint is
  computed.

## Consequences

A no-change commit reads each in-scope file once instead of twice, and
out-of-scope files are never read. JSON consumers that read the removed
fields break; the project is pre-release, and `docs/json.md` records the
change.

The kept primitives have no caller outside tests. Whether the future
cache trusts `stat` data alone (needing a racy-timestamp guard like
Git's) or keeps ADR-0012's full-content hash is left to the tree design.
