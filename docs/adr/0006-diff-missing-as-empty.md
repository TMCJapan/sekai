# ADR-0006: diff treats missing chunks as empty

- Status: Accepted
- Date: 2026-09-14

## Context

`entities/`/`poi` files are sparse: a coordinate present in `region/`
routinely has no entry there. `diff` errored on the missing side
(`ChunkNotFound*`), so diffing sparse kinds systematically failed while
the identical `region` coordinate succeeded.

## Decision

A chunk absent (or tombstoned) on a side diffs as an empty compound:
absent on both sides yields no entries; present on one side reports
whole-value `Added`/`Removed` entries. Missing coordinates are never
errors. Still loud: corrupt payloads, CAS-missing blobs, unreadable
files. The two `ChunkNotFound*` error variants were deleted.

## Consequences

Sparse-kind diffs work; chunk appearance/disappearance between
snapshots reports as whole-value entries (documented in
`docs/json.md`). Shelved separately: per-kind volatile-ignore sets,
`.mcc` external bodies.
