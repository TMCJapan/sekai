# ADR-0003: Scope as filter, `--in` selection syntax

- Status: Accepted
- Date: 2026-09-12

## Context

Backup/rollback/diff need sub-world operation (one dimension, a chunk
rectangle, explicit chunks) without partitioning history. Early flags
(`--chunk`/`--region`/`--dimension`/`--dim`) were mutually exclusive
and could not express multi-dimension or rectangle selections.

## Decision

Scope is a filter, not a partition: `Scope::{World, Select{kinds,
areas}}` with `Area::{All, Rect, Chunks}` per dimension; kinds apply
uniformly. Out-of-scope coordinates record no rows and no tombstones
(fallback resolves them); rollback never touches out-of-scope files.
CLI takes repeatable `--in DIM[:x,z|x0,z0..x1,z1]` plus `--region
DIM:RX,RZ` sugar and repeatable `--kind` (empty means all); empty
selection is the whole world. Rectangles expand fully with no size
cap. One shared owned type (no borrowed mirror) so worker tasks can
clone it.

## Consequences

Scoped→full backup sequences record no spurious tombstones
(property-tested). Rectangle enumeration cost is the caller's
responsibility, documented in `docs/json.md`.
